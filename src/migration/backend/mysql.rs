use std::{
    collections::{BTreeMap, HashMap},
    hash::{DefaultHasher, Hash, Hasher},
};

use anyhow::{Context, Result, bail};
use toasty::{
    Connection, Db, Executor,
    schema::db::{self, EnumVariant, TypeEnum},
    stmt::{self, Value},
};

use super::{
    AppliedIdsFuture, ApplyFuture, BackendId, BackendMigration, DdlAtomicity, InspectFuture,
    LedgerMigration, MigrationBackend, ObservedColumn, ObservedIndex, ObservedSchema,
    ObservedTable, PrepareLedgerFuture, RollbackFuture, SchemaInspectRequest, normalize_observed,
};
use crate::migration::SchemaScope;

pub struct MySqlMigrationBackend {
    id: BackendId,
}

impl Default for MySqlMigrationBackend {
    fn default() -> Self {
        Self {
            id: BackendId("mysql".to_owned()),
        }
    }
}

impl MigrationBackend for MySqlMigrationBackend {
    fn backend_id(&self) -> &BackendId {
        &self.id
    }

    fn aliases(&self) -> &[&str] {
        &["mysql"]
    }

    fn ddl_atomicity(&self) -> DdlAtomicity {
        DdlAtomicity::ImplicitCommit
    }

    fn inspect<'a>(&'a self, request: SchemaInspectRequest<'a>) -> InspectFuture<'a> {
        Box::pin(async move { inspect(request).await })
    }

    fn normalize(&self, observed: &ObservedSchema, target: &db::Schema) -> Result<db::Schema> {
        normalize_observed(observed, target, |column, target| {
            let inferred = mysql_type(column)?;
            Ok(target
                .filter(|target| mysql_storage_equivalent(column, &target.storage_ty))
                .map(|target| (target.ty.clone(), target.storage_ty.clone()))
                .unwrap_or(inferred))
        })
    }

    fn inspect_applied_ids<'a>(
        &'a self,
        source_code: &'a str,
        tracked: &'a [LedgerMigration],
        db: &'a mut Db,
    ) -> AppliedIdsFuture<'a> {
        Box::pin(async move { inspect_applied_ids(source_code, tracked, db).await })
    }

    fn prepare_ledger<'a>(
        &'a self,
        source_code: &'a str,
        tracked: &'a [LedgerMigration],
        db: &'a mut Db,
    ) -> PrepareLedgerFuture<'a> {
        Box::pin(async move { prepare_ledger(source_code, tracked, db).await })
    }

    fn apply_migration<'a>(
        &'a self,
        migration: BackendMigration,
        db: &'a mut Db,
    ) -> ApplyFuture<'a> {
        Box::pin(async move { apply_migration(migration, true, db).await })
    }

    fn record_migration<'a>(
        &'a self,
        migration: BackendMigration,
        db: &'a mut Db,
    ) -> ApplyFuture<'a> {
        Box::pin(async move { apply_migration(migration, false, db).await })
    }

    fn rollback_migration<'a>(
        &'a self,
        migration: BackendMigration,
        db: &'a mut Db,
    ) -> RollbackFuture<'a> {
        Box::pin(async move { rollback_migration(migration, db).await })
    }
}

const LEDGER: &str = "__toasty_mgr_migrations";
const RUNS: &str = "__toasty_mgr_migration_runs";
const LOCK_TIMEOUT_SECONDS: i64 = 30;
const CREATE_LEDGER_SQL: &str = "CREATE TABLE IF NOT EXISTS __toasty_mgr_migrations (\
    source_code VARCHAR(191) NOT NULL, id BIGINT UNSIGNED NOT NULL, name VARCHAR(255) NOT NULL, \
    applied_at TIMESTAMP(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6), \
    PRIMARY KEY (source_code, id)) ENGINE=InnoDB";
const CREATE_RUNS_SQL: &str = "CREATE TABLE IF NOT EXISTS __toasty_mgr_migration_runs (\
    source_code VARCHAR(191) NOT NULL, id BIGINT UNSIGNED NOT NULL, name VARCHAR(255) NOT NULL, \
    direction VARCHAR(16) NOT NULL, statement_index INT UNSIGNED NOT NULL DEFAULT 0, \
    started_at TIMESTAMP(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6), \
    PRIMARY KEY (source_code, id, direction)) ENGINE=InnoDB";

async fn inspect_applied_ids(
    source_code: &str,
    tracked: &[LedgerMigration],
    db: &mut Db,
) -> Result<Vec<u64>> {
    if table_exists(db, RUNS).await? {
        ensure_no_recovery_marker(source_code, db).await?;
    }
    if !table_exists(db, LEDGER).await? {
        return Ok(Vec::new());
    }
    let tracked = tracked_by_id(tracked);
    let mut applied = Vec::new();
    for (id, name) in composite_rows(source_code, db).await? {
        validate_tracked_name(&tracked, id, &name, "migration ledger")?;
        applied.push(id);
    }
    Ok(applied)
}

async fn prepare_ledger(
    source_code: &str,
    tracked: &[LedgerMigration],
    db: &mut Db,
) -> Result<Vec<u64>> {
    let tracked = tracked_by_id(tracked);
    let mut connection = db.connection().await?;
    let lock = acquire_lock(source_code, &mut connection).await?;
    let result = async {
        create_metadata_tables(&mut connection).await?;
        ensure_no_recovery_marker(source_code, &mut connection).await?;
        let rows = composite_rows(source_code, &mut connection).await?;
        let mut ids = Vec::with_capacity(rows.len());
        for (id, name) in rows {
            validate_tracked_name(&tracked, id, &name, "migration ledger")?;
            ids.push(id);
        }
        Ok(ids)
    }
    .await;
    finish_lock(source_code, &lock, &mut connection, result).await
}

async fn apply_migration(
    migration: BackendMigration,
    execute_sql: bool,
    db: &mut Db,
) -> Result<bool> {
    let mut connection = db.connection().await?;
    let lock = acquire_lock(&migration.source_code, &mut connection).await?;
    let result = async {
        create_metadata_tables(&mut connection).await?;
        ensure_no_recovery_marker(&migration.source_code, &mut connection).await?;
        if let Some(name) =
            ledger_name(&migration.source_code, migration.id, &mut connection).await?
        {
            if name != migration.name {
                bail!(
                    "migration ledger name mismatch for {}:{}: expected {}, found {name}",
                    migration.source_code,
                    migration.id,
                    migration.name
                );
            }
            return Ok(false);
        }
        if execute_sql {
            create_run_marker(&migration, "apply", &mut connection).await?;
            execute_with_progress(&migration, "apply", &mut connection).await?;
        }
        toasty::sql::statement(
            "INSERT INTO __toasty_mgr_migrations (source_code, id, name) VALUES (?, ?, ?)",
        )
        .bind(migration.source_code.clone())
        .bind(migration.id)
        .bind(migration.name.clone())
        .exec(&mut connection)
        .await?;
        if execute_sql {
            delete_run_marker(&migration, "apply", &mut connection).await?;
        }
        Ok(true)
    }
    .await;
    finish_lock(&migration.source_code, &lock, &mut connection, result).await
}

async fn rollback_migration(migration: BackendMigration, db: &mut Db) -> Result<()> {
    let mut connection = db.connection().await?;
    let lock = acquire_lock(&migration.source_code, &mut connection).await?;
    let result = async {
        create_metadata_tables(&mut connection).await?;
        ensure_no_recovery_marker(&migration.source_code, &mut connection).await?;
        let rows = toasty::sql::query(
            "SELECT id, name FROM __toasty_mgr_migrations \
             WHERE source_code = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(migration.source_code.clone())
        .exec(&mut connection)
        .await?;
        let Some((id, name)) = rows.first().map(ledger_row).transpose()? else {
            bail!(
                "migration rollback source has no applied migrations: {}",
                migration.source_code
            );
        };
        if id != migration.id || name != migration.name {
            bail!(
                "migration rollback order changed for {}: expected {} {}, found {id} {name}",
                migration.source_code,
                migration.id,
                migration.name
            );
        }
        create_run_marker(&migration, "rollback", &mut connection).await?;
        execute_with_progress(&migration, "rollback", &mut connection).await?;
        toasty::sql::statement(
            "DELETE FROM __toasty_mgr_migrations WHERE source_code = ? AND id = ?",
        )
        .bind(migration.source_code.clone())
        .bind(migration.id)
        .exec(&mut connection)
        .await?;
        delete_run_marker(&migration, "rollback", &mut connection).await?;
        Ok(())
    }
    .await;
    finish_lock(&migration.source_code, &lock, &mut connection, result).await
}

async fn acquire_lock(source_code: &str, connection: &mut Connection) -> Result<String> {
    let rows = toasty::sql::query("SELECT DATABASE()")
        .exec(&mut *connection)
        .await?;
    let database = value_string(
        rows.first()
            .and_then(first_value)
            .context("MySQL returned no current database for migration lock")?,
    )?;
    let lock = lock_name(&database);
    // A cancelled task may return a pooled MySQL connection with a session lock still held.
    // Only this database-scoped migration lock belongs to the adapter; business locks are untouched.
    toasty::sql::query("SELECT RELEASE_LOCK(?)")
        .bind(lock.clone())
        .exec(&mut *connection)
        .await?;
    let rows = toasty::sql::query("SELECT GET_LOCK(?, ?)")
        .bind(lock.clone())
        .bind(LOCK_TIMEOUT_SECONDS)
        .exec(&mut *connection)
        .await?;
    if rows
        .first()
        .and_then(first_value)
        .map(value_i64)
        .transpose()?
        != Some(1)
    {
        bail!("migration_lock_timeout: {source_code}");
    }
    Ok(lock)
}

async fn finish_lock<T>(
    source_code: &str,
    lock: &str,
    connection: &mut Connection,
    result: Result<T>,
) -> Result<T> {
    let released: Result<()> = match toasty::sql::query("SELECT RELEASE_LOCK(?)")
        .bind(lock.to_owned())
        .exec(connection)
        .await
    {
        Ok(rows) => rows
            .first()
            .and_then(first_value)
            .context("MySQL RELEASE_LOCK returned no value")
            .and_then(value_i64)
            .and_then(|value| {
                if value == 1 {
                    Ok(())
                } else {
                    bail!("MySQL migration lock was not owned: {source_code}")
                }
            }),
        Err(error) => Err(error.into()),
    };
    match (result, released) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(release_error)) => Err(error.context(format!(
            "failed to release MySQL migration lock: {release_error}"
        ))),
    }
}

fn lock_name(database: &str) -> String {
    let mut hasher = DefaultHasher::new();
    database.hash(&mut hasher);
    format!("toasty-mgr:{:016x}", hasher.finish())
}

async fn create_metadata_tables(executor: &mut dyn Executor) -> Result<()> {
    toasty::sql::statement(CREATE_LEDGER_SQL)
        .exec(executor)
        .await?;
    toasty::sql::statement(CREATE_RUNS_SQL)
        .exec(executor)
        .await?;
    Ok(())
}

async fn ensure_no_recovery_marker(source_code: &str, executor: &mut dyn Executor) -> Result<()> {
    let rows = toasty::sql::query(
        "SELECT id, name, direction, statement_index FROM __toasty_mgr_migration_runs \
         WHERE source_code = ? ORDER BY started_at LIMIT 1",
    )
    .bind(source_code.to_owned())
    .exec(executor)
    .await?;
    if let Some(row) = rows.first() {
        let values = record(row, 4, "MySQL migration recovery marker")?;
        bail!(
            "migration_recovery_ambiguous: {}:{} {} {} statement {}",
            source_code,
            value_u64(&values[0])?,
            value_string(&values[1])?,
            value_string(&values[2])?,
            value_u64(&values[3])?
        );
    }
    Ok(())
}

async fn create_run_marker(
    migration: &BackendMigration,
    direction: &str,
    executor: &mut dyn Executor,
) -> Result<()> {
    toasty::sql::statement(
        "INSERT INTO __toasty_mgr_migration_runs \
         (source_code, id, name, direction, statement_index) VALUES (?, ?, ?, ?, 0)",
    )
    .bind(migration.source_code.clone())
    .bind(migration.id)
    .bind(migration.name.clone())
    .bind(direction.to_owned())
    .exec(executor)
    .await?;
    Ok(())
}

async fn execute_with_progress(
    migration: &BackendMigration,
    direction: &str,
    executor: &mut dyn Executor,
) -> Result<()> {
    let sql = db::Migration::new_sql(migration.sql.clone());
    for (index, statement) in sql.statements().iter().enumerate() {
        toasty::sql::statement(
            "UPDATE __toasty_mgr_migration_runs SET statement_index = ? \
             WHERE source_code = ? AND id = ? AND direction = ?",
        )
        .bind(u64::try_from(index)?)
        .bind(migration.source_code.clone())
        .bind(migration.id)
        .bind(direction.to_owned())
        .exec(executor)
        .await?;
        toasty::sql::statement(statement.to_owned())
            .exec(executor)
            .await?;
    }
    Ok(())
}

async fn delete_run_marker(
    migration: &BackendMigration,
    direction: &str,
    executor: &mut dyn Executor,
) -> Result<()> {
    toasty::sql::statement(
        "DELETE FROM __toasty_mgr_migration_runs \
         WHERE source_code = ? AND id = ? AND direction = ?",
    )
    .bind(migration.source_code.clone())
    .bind(migration.id)
    .bind(direction.to_owned())
    .exec(executor)
    .await?;
    Ok(())
}

async fn table_exists(executor: &mut dyn Executor, table: &str) -> Result<bool> {
    let rows = toasty::sql::query(
        "SELECT 1 FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? LIMIT 1",
    )
    .bind(table.to_owned())
    .exec(executor)
    .await?;
    Ok(!rows.is_empty())
}

async fn ledger_name(
    source_code: &str,
    id: u64,
    executor: &mut dyn Executor,
) -> Result<Option<String>> {
    let rows = toasty::sql::query(
        "SELECT name FROM __toasty_mgr_migrations WHERE source_code = ? AND id = ?",
    )
    .bind(source_code.to_owned())
    .bind(id)
    .exec(executor)
    .await?;
    rows.first()
        .and_then(first_value)
        .map(value_string)
        .transpose()
}

async fn composite_rows(
    source_code: &str,
    executor: &mut dyn Executor,
) -> Result<Vec<(u64, String)>> {
    toasty::sql::query(
        "SELECT id, name FROM __toasty_mgr_migrations WHERE source_code = ? ORDER BY id",
    )
    .bind(source_code.to_owned())
    .exec(executor)
    .await?
    .iter()
    .map(ledger_row)
    .collect()
}

fn ledger_row(row: &Value) -> Result<(u64, String)> {
    let values = record(row, 2, "migration ledger")?;
    Ok((value_u64(&values[0])?, value_string(&values[1])?))
}

fn tracked_by_id(tracked: &[LedgerMigration]) -> HashMap<u64, String> {
    tracked
        .iter()
        .map(|migration| (migration.id, migration.name.clone()))
        .collect()
}

fn validate_tracked_name(
    tracked: &HashMap<u64, String>,
    id: u64,
    name: &str,
    origin: &str,
) -> Result<()> {
    let expected = tracked
        .get(&id)
        .with_context(|| format!("unknown applied migration id in {origin}: {id}"))?;
    if expected != name {
        bail!("migration ledger name mismatch for id {id}: expected {expected}, found {name}");
    }
    Ok(())
}

async fn inspect(request: SchemaInspectRequest<'_>) -> Result<ObservedSchema> {
    let namespace = match request.namespace {
        Some(value) => value.to_owned(),
        None => {
            let rows = toasty::sql::query("SELECT DATABASE()")
                .exec(request.db)
                .await?;
            value_string(
                rows.first()
                    .and_then(first_value)
                    .context("MySQL returned no current database")?,
            )?
        }
    };
    let column_rows = toasty::sql::query(
        "SELECT c.TABLE_NAME, c.COLUMN_NAME, c.DATA_TYPE, c.COLUMN_TYPE, c.IS_NULLABLE, \
         c.COLUMN_DEFAULT, c.ORDINAL_POSITION, c.COLUMN_COMMENT, c.EXTRA, t.TABLE_COMMENT \
         FROM information_schema.COLUMNS c \
         JOIN information_schema.TABLES t ON t.TABLE_SCHEMA = c.TABLE_SCHEMA \
          AND t.TABLE_NAME = c.TABLE_NAME AND t.TABLE_TYPE = 'BASE TABLE' \
         WHERE c.TABLE_SCHEMA = ? ORDER BY c.TABLE_NAME, c.ORDINAL_POSITION",
    )
    .bind(namespace.clone())
    .exec(request.db)
    .await?;
    let index_rows = toasty::sql::query(
        "SELECT TABLE_NAME, INDEX_NAME, NON_UNIQUE, SEQ_IN_INDEX, COLUMN_NAME, SUB_PART \
         FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? \
         ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX",
    )
    .bind(namespace.clone())
    .exec(request.db)
    .await?;
    let constraint_rows = toasty::sql::query(
        "SELECT TABLE_NAME, CONSTRAINT_NAME, CONSTRAINT_TYPE \
         FROM information_schema.TABLE_CONSTRAINTS WHERE CONSTRAINT_SCHEMA = ? \
          AND CONSTRAINT_TYPE IN ('FOREIGN KEY', 'CHECK') ORDER BY TABLE_NAME, CONSTRAINT_NAME",
    )
    .bind(namespace.clone())
    .exec(request.db)
    .await?;
    let mut tables = BTreeMap::<String, ObservedTable>::new();
    let mut diagnostics = Vec::new();
    for row in column_rows {
        let values = record(&row, 10, "MySQL column catalog")?;
        let table_name = value_string(&values[0])?;
        if is_internal_table(&table_name) || !is_managed(&table_name, &request) {
            continue;
        }
        let column_name = value_string(&values[1])?;
        let extra = value_string(&values[8])?;
        if extra.to_ascii_uppercase().contains("GENERATED") {
            diagnostics.push(format!("mysql_generated_column:{table_name}.{column_name}"));
        }
        tables
            .entry(table_name.clone())
            .or_insert_with(|| ObservedTable {
                name: table_name,
                comment: nonempty(value_string(&values[9]).unwrap_or_default()),
                columns: Vec::new(),
                indices: Vec::new(),
            })
            .columns
            .push(ObservedColumn {
                name: column_name,
                data_type: value_string(&values[2])?.to_ascii_lowercase(),
                native_type: value_string(&values[3])?.to_ascii_lowercase(),
                nullable: value_string(&values[4])? == "YES",
                auto_increment: extra.to_ascii_lowercase().contains("auto_increment"),
                default: value_optional_string(&values[5])?,
                ordinal: usize::try_from(value_u64(&values[6])?)?,
                comment: nonempty(value_string(&values[7])?),
            });
    }
    let mut grouped_indices = BTreeMap::<(String, String), (bool, Vec<(u64, String)>)>::new();
    for row in index_rows {
        let values = record(&row, 6, "MySQL index catalog")?;
        let table_name = value_string(&values[0])?;
        if !tables.contains_key(&table_name) {
            continue;
        }
        let index_name = value_string(&values[1])?;
        if !matches!(values[5], Value::Null) {
            diagnostics.push(format!("mysql_prefix_index:{table_name}.{index_name}"));
        }
        let Some(column_name) = value_optional_string(&values[4])? else {
            diagnostics.push(format!("mysql_expression_index:{table_name}.{index_name}"));
            continue;
        };
        let entry = grouped_indices
            .entry((table_name, index_name))
            .or_insert_with(|| (value_u64(&values[2]).unwrap_or(1) == 0, Vec::new()));
        entry.1.push((value_u64(&values[3])?, column_name));
    }
    for ((table_name, index_name), (unique, mut columns)) in grouped_indices {
        columns.sort_by_key(|(ordinal, _)| *ordinal);
        tables
            .get_mut(&table_name)
            .context("MySQL index references an unknown table")?
            .indices
            .push(ObservedIndex {
                primary_key: index_name == "PRIMARY",
                name: index_name,
                columns: columns.into_iter().map(|(_, name)| name).collect(),
                unique,
            });
    }
    for row in constraint_rows {
        let values = record(&row, 3, "MySQL constraint catalog")?;
        let table_name = value_string(&values[0])?;
        if tables.contains_key(&table_name) {
            diagnostics.push(format!(
                "mysql_{}_constraint:{table_name}.{}",
                value_string(&values[2])?
                    .to_ascii_lowercase()
                    .replace(' ', "_"),
                value_string(&values[1])?
            ));
        }
    }
    Ok(ObservedSchema {
        namespace,
        tables: tables.into_values().collect(),
        diagnostics,
    })
}

fn mysql_type(column: &ObservedColumn) -> Result<(stmt::Type, db::Type)> {
    let native = column.native_type.as_str();
    let unsigned = native.contains(" unsigned");
    Ok(match column.data_type.as_str() {
        "tinyint" if native.starts_with("tinyint(1)") && !unsigned => {
            (stmt::Type::Bool, db::Type::Boolean)
        }
        "tinyint" => integer_type(1, unsigned),
        "smallint" => integer_type(2, unsigned),
        "mediumint" | "int" | "integer" => integer_type(4, unsigned),
        "bigint" => integer_type(8, unsigned),
        "float" => (stmt::Type::F32, db::Type::Float(4)),
        "double" | "real" => (stmt::Type::F64, db::Type::Float(8)),
        "decimal" | "numeric" => {
            let (precision, scale) = parse_precision_scale(native)?;
            (
                stmt::Type::Decimal,
                db::Type::Numeric(Some((precision, scale))),
            )
        }
        "varchar" => (
            stmt::Type::String,
            db::Type::VarChar(parse_single_size(native, "varchar")?),
        ),
        "text" => (stmt::Type::String, db::Type::Text),
        "binary" => (
            stmt::Type::Bytes,
            db::Type::Binary(u8::try_from(parse_single_size(native, "binary")?)?),
        ),
        "blob" => (stmt::Type::Bytes, db::Type::Blob),
        "date" => (stmt::Type::Date, db::Type::Date),
        "time" => (
            stmt::Type::Time,
            db::Type::Time(parse_temporal_precision(native, "time")?),
        ),
        "datetime" => (
            stmt::Type::DateTime,
            db::Type::DateTime(parse_temporal_precision(native, "datetime")?),
        ),
        "timestamp" => (
            stmt::Type::Timestamp,
            db::Type::Timestamp(parse_temporal_precision(native, "timestamp")?),
        ),
        "json" => (stmt::Type::Object, db::Type::Json),
        "enum" => (
            stmt::Type::String,
            db::Type::Enum(TypeEnum {
                name: None,
                variants: parse_mysql_enum(native)?
                    .into_iter()
                    .map(|name| EnumVariant { name })
                    .collect(),
            }),
        ),
        other => bail!(
            "unsupported_mysql_column_type: {} ({other}/{native})",
            column.name
        ),
    })
}

fn integer_type(size: u8, unsigned: bool) -> (stmt::Type, db::Type) {
    if unsigned {
        let ty = match size {
            1 => stmt::Type::U8,
            2 => stmt::Type::U16,
            3..=4 => stmt::Type::U32,
            _ => stmt::Type::U64,
        };
        (ty, db::Type::UnsignedInteger(size))
    } else {
        let ty = match size {
            1 => stmt::Type::I8,
            2 => stmt::Type::I16,
            3..=4 => stmt::Type::I32,
            _ => stmt::Type::I64,
        };
        (ty, db::Type::Integer(size))
    }
}

fn mysql_storage_equivalent(column: &ObservedColumn, target: &db::Type) -> bool {
    let native = column.native_type.as_str();
    let inferred = mysql_type(column).ok().map(|(_, storage)| storage);
    if inferred.as_ref() == Some(target) {
        return true;
    }
    match target {
        db::Type::Document { .. } | db::Type::List(_) | db::Type::Json => {
            column.data_type == "json"
        }
        db::Type::VarChar(size) => {
            parse_single_size(native, "varchar").is_ok_and(|value| value == *size)
        }
        db::Type::Enum(target_enum) if column.data_type == "enum" => parse_mysql_enum(native)
            .is_ok_and(|values| {
                values
                    == target_enum
                        .variants
                        .iter()
                        .map(|variant| variant.name.clone())
                        .collect::<Vec<_>>()
            }),
        _ => false,
    }
}

fn parse_single_size(native: &str, name: &str) -> Result<u64> {
    native
        .strip_prefix(name)
        .and_then(|value| value.strip_prefix('('))
        .and_then(|value| value.split_once(')'))
        .map(|(size, _)| size)
        .context("MySQL type is missing a size")?
        .parse()
        .context("MySQL type has an invalid size")
}

fn parse_precision_scale(native: &str) -> Result<(u32, u32)> {
    let values = native
        .split_once('(')
        .and_then(|(_, value)| value.split_once(')'))
        .map(|(value, _)| value)
        .context("MySQL decimal type is missing precision")?;
    let (precision, scale) = values.split_once(',').unwrap_or((values, "0"));
    Ok((precision.trim().parse()?, scale.trim().parse()?))
}

fn parse_temporal_precision(native: &str, name: &str) -> Result<u8> {
    if native == name {
        return Ok(0);
    }
    Ok(u8::try_from(parse_single_size(native, name)?)?)
}

fn parse_mysql_enum(native: &str) -> Result<Vec<String>> {
    let body = native
        .strip_prefix("enum(")
        .and_then(|value| value.strip_suffix(')'))
        .context("MySQL enum type has an invalid declaration")?;
    let mut values = Vec::new();
    let mut chars = body.chars().peekable();
    while chars.peek().is_some() {
        while chars
            .peek()
            .is_some_and(|ch| ch.is_ascii_whitespace() || *ch == ',')
        {
            chars.next();
        }
        if chars.next() != Some('\'') {
            bail!("MySQL enum value must start with a quote");
        }
        let mut value = String::new();
        loop {
            match chars.next() {
                Some('\\') => value.push(
                    chars
                        .next()
                        .context("MySQL enum has an incomplete escape")?,
                ),
                Some('\'') if chars.peek() == Some(&'\'') => {
                    chars.next();
                    value.push('\'');
                }
                Some('\'') => break,
                Some(ch) => value.push(ch),
                None => bail!("MySQL enum has an unterminated value"),
            }
        }
        values.push(value);
    }
    if values.is_empty() {
        bail!("MySQL enum must contain at least one value");
    }
    Ok(values)
}

fn is_managed(name: &str, request: &SchemaInspectRequest<'_>) -> bool {
    match request.scope {
        SchemaScope::Managed => request.managed_tables.iter().any(|table| table == name),
        SchemaScope::Tables(tables) => tables.contains(name),
        SchemaScope::Prefixes(prefixes) => prefixes.iter().any(|prefix| name.starts_with(prefix)),
        SchemaScope::NamespaceExclusive => true,
    }
}

fn is_internal_table(name: &str) -> bool {
    matches!(name, "__toasty_migrations" | LEDGER | RUNS)
}

fn record<'a>(row: &'a Value, minimum: usize, origin: &str) -> Result<&'a [Value]> {
    let Value::Record(values) = row else {
        bail!("{origin} returned a non-record row");
    };
    if values.len() < minimum {
        bail!("{origin} row has an invalid column count");
    }
    Ok(values)
}

fn first_value(row: &Value) -> Option<&Value> {
    match row {
        Value::Record(record) => record.first(),
        _ => None,
    }
}

fn value_optional_string(value: &Value) -> Result<Option<String>> {
    match value {
        Value::String(value) => Ok(Some(value.clone())),
        Value::Null => Ok(None),
        other => bail!("expected optional string catalog value, got {other:?}"),
    }
}

fn value_string(value: &Value) -> Result<String> {
    value_optional_string(value)?.context("expected non-null string catalog value")
}

fn value_i64(value: &Value) -> Result<i64> {
    match value {
        Value::I8(value) => Ok(i64::from(*value)),
        Value::I16(value) => Ok(i64::from(*value)),
        Value::I32(value) => Ok(i64::from(*value)),
        Value::I64(value) => Ok(*value),
        Value::U8(value) => Ok(i64::from(*value)),
        Value::U16(value) => Ok(i64::from(*value)),
        Value::U32(value) => Ok(i64::from(*value)),
        Value::U64(value) => i64::try_from(*value).context("MySQL integer exceeds i64"),
        other => bail!("expected integer catalog value, got {other:?}"),
    }
}

fn value_u64(value: &Value) -> Result<u64> {
    match value {
        Value::U8(value) => Ok(u64::from(*value)),
        Value::U16(value) => Ok(u64::from(*value)),
        Value::U32(value) => Ok(u64::from(*value)),
        Value::U64(value) => Ok(*value),
        _ => u64::try_from(value_i64(value)?).context("MySQL integer is negative"),
    }
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use toasty::schema::db;

    use super::{
        CREATE_LEDGER_SQL, CREATE_RUNS_SQL, mysql_storage_equivalent, parse_mysql_enum,
        parse_precision_scale,
    };
    use crate::migration::ObservedColumn;

    fn column(data_type: &str, native_type: &str) -> ObservedColumn {
        ObservedColumn {
            name: "value".to_owned(),
            data_type: data_type.to_owned(),
            native_type: native_type.to_owned(),
            nullable: false,
            auto_increment: false,
            default: None,
            ordinal: 0,
            comment: None,
        }
    }

    #[test]
    fn mysql_native_types_and_enums_are_parsed_without_a_sql_parser_dependency() {
        assert_eq!(parse_precision_scale("decimal(18,4)").unwrap(), (18, 4));
        assert_eq!(
            parse_mysql_enum("enum('new','can\\'t','it''s')").unwrap(),
            ["new", "can't", "it's"]
        );
        assert!(mysql_storage_equivalent(
            &column("json", "json"),
            &db::Type::Document { binary: true }
        ));
        assert!(mysql_storage_equivalent(
            &column("varchar", "varchar(191)"),
            &db::Type::VarChar(191)
        ));
    }

    #[test]
    fn mysql_metadata_tables_are_source_scoped_without_foreign_keys() {
        assert!(CREATE_LEDGER_SQL.contains("PRIMARY KEY (source_code, id)"));
        assert!(CREATE_RUNS_SQL.contains("PRIMARY KEY (source_code, id, direction)"));
        assert!(!CREATE_LEDGER_SQL.contains("REFERENCES"));
        assert!(!CREATE_RUNS_SQL.contains("REFERENCES"));
    }
}
