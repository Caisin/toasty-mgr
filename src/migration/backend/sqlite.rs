use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, bail};
use toasty::{
    Connection, Db, Executor,
    schema::db,
    stmt::{self, Value},
};

use super::{
    AppliedIdsFuture, ApplyFuture, BackendId, BackendMigration, DdlAtomicity, InspectFuture,
    LedgerMigration, MigrationBackend, ObservedColumn, ObservedIndex, ObservedSchema,
    ObservedTable, PrepareLedgerFuture, RollbackFuture, SchemaInspectRequest, normalize_observed,
};
use crate::migration::SchemaScope;

pub struct SqliteMigrationBackend {
    id: BackendId,
}

impl Default for SqliteMigrationBackend {
    fn default() -> Self {
        Self {
            id: BackendId("sqlite".to_owned()),
        }
    }
}

impl MigrationBackend for SqliteMigrationBackend {
    fn backend_id(&self) -> &BackendId {
        &self.id
    }

    fn aliases(&self) -> &[&str] {
        &["sqlite"]
    }

    fn ddl_atomicity(&self) -> DdlAtomicity {
        DdlAtomicity::Transactional
    }

    fn inspect<'a>(&'a self, request: SchemaInspectRequest<'a>) -> InspectFuture<'a> {
        Box::pin(async move { inspect(request).await })
    }

    fn normalize(&self, observed: &ObservedSchema, target: &db::Schema) -> Result<db::Schema> {
        normalize(observed, target)
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
const CREATE_LEDGER_SQL: &str = "CREATE TABLE IF NOT EXISTS __toasty_mgr_migrations (\
    source_code TEXT NOT NULL, id INTEGER NOT NULL CHECK (id >= 0), name TEXT NOT NULL, \
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY (source_code, id))";

pub(super) async fn inspect_applied_ids(
    source_code: &str,
    tracked: &[LedgerMigration],
    db: &mut Db,
) -> Result<Vec<u64>> {
    let tracked = tracked_by_id(tracked);
    if !table_exists(db, LEDGER).await? {
        return Ok(Vec::new());
    }
    let mut applied = Vec::new();
    for (id, name) in composite_rows(source_code, db).await? {
        validate_tracked_name(&tracked, id, &name, "migration ledger")?;
        applied.push(id);
    }
    Ok(applied)
}

pub(super) async fn prepare_ledger(
    source_code: &str,
    tracked: &[LedgerMigration],
    db: &mut Db,
) -> Result<Vec<u64>> {
    let tracked = tracked_by_id(tracked);
    let mut connection = db.connection().await?;
    begin_immediate(&mut connection).await?;
    let result = async {
        create_ledger(&mut connection).await?;
        let rows = composite_rows(source_code, &mut connection).await?;
        let mut ids = Vec::with_capacity(rows.len());
        for (id, name) in rows {
            validate_tracked_name(&tracked, id, &name, "migration ledger")?;
            ids.push(id);
        }
        Ok(ids)
    }
    .await;
    finish_transaction(&mut connection, result).await
}

pub(super) async fn apply_migration(
    migration: BackendMigration,
    execute_sql: bool,
    db: &mut Db,
) -> Result<bool> {
    let mut connection = db.connection().await?;
    begin_immediate(&mut connection).await?;
    let result = async {
        create_ledger(&mut connection).await?;
        let existing = toasty::sql::query(
            "SELECT name FROM __toasty_mgr_migrations WHERE source_code = ?1 AND id = ?2",
        )
        .bind(migration.source_code.clone())
        .bind(to_i64(migration.id)?)
        .exec(&mut connection)
        .await?;
        if let Some(name) = existing.first().and_then(first_value) {
            let name = value_string(name)?;
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
            execute_sql_statements(&migration.sql, &mut connection).await?;
        }
        toasty::sql::statement(
            "INSERT INTO __toasty_mgr_migrations (source_code, id, name) VALUES (?1, ?2, ?3)",
        )
        .bind(migration.source_code)
        .bind(to_i64(migration.id)?)
        .bind(migration.name)
        .exec(&mut connection)
        .await?;
        Ok(true)
    }
    .await;
    finish_transaction(&mut connection, result).await
}

pub(super) async fn rollback_migration(migration: BackendMigration, db: &mut Db) -> Result<()> {
    let mut connection = db.connection().await?;
    begin_immediate(&mut connection).await?;
    let result = async {
        create_ledger(&mut connection).await?;
        let rows = toasty::sql::query(
            "SELECT id, name FROM __toasty_mgr_migrations \
             WHERE source_code = ?1 ORDER BY id DESC LIMIT 1",
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
        execute_sql_statements(&migration.sql, &mut connection).await?;
        toasty::sql::statement(
            "DELETE FROM __toasty_mgr_migrations WHERE source_code = ?1 AND id = ?2",
        )
        .bind(migration.source_code)
        .bind(to_i64(migration.id)?)
        .exec(&mut connection)
        .await?;
        Ok(())
    }
    .await;
    finish_transaction(&mut connection, result).await
}

async fn begin_immediate(connection: &mut Connection) -> Result<()> {
    toasty::sql::statement("BEGIN IMMEDIATE")
        .exec(connection)
        .await?;
    Ok(())
}

async fn finish_transaction<T>(connection: &mut Connection, result: Result<T>) -> Result<T> {
    match result {
        Ok(value) => {
            toasty::sql::statement("COMMIT").exec(connection).await?;
            Ok(value)
        }
        Err(error) => {
            if let Err(rollback_error) = toasty::sql::statement("ROLLBACK").exec(connection).await {
                return Err(error.context(format!(
                    "failed to rollback SQLite-family migration transaction: {rollback_error}"
                )));
            }
            Err(error)
        }
    }
}

async fn create_ledger(executor: &mut dyn Executor) -> Result<()> {
    toasty::sql::statement(CREATE_LEDGER_SQL)
        .exec(executor)
        .await?;
    Ok(())
}

async fn table_exists(executor: &mut dyn Executor, table: &str) -> Result<bool> {
    let rows = toasty::sql::query(
        "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1 LIMIT 1",
    )
    .bind(table.to_owned())
    .exec(executor)
    .await?;
    Ok(!rows.is_empty())
}

async fn composite_rows(
    source_code: &str,
    executor: &mut dyn Executor,
) -> Result<Vec<(u64, String)>> {
    toasty::sql::query(
        "SELECT id, name FROM __toasty_mgr_migrations WHERE source_code = ?1 ORDER BY id",
    )
    .bind(source_code.to_owned())
    .exec(executor)
    .await?
    .iter()
    .map(ledger_row)
    .collect()
}

fn ledger_row(row: &Value) -> Result<(u64, String)> {
    let Value::Record(values) = row else {
        bail!("migration ledger returned a non-record row");
    };
    let [id, name] = values.as_slice() else {
        bail!("migration ledger row has an invalid column count");
    };
    Ok((
        u64::try_from(value_i64(id)?).context("migration ledger contains a negative id")?,
        value_string(name)?,
    ))
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

async fn execute_sql_statements(sql: &str, executor: &mut dyn Executor) -> Result<()> {
    let migration = db::Migration::new_sql(sql.to_owned());
    for statement in migration.statements() {
        toasty::sql::statement(statement.to_owned())
            .exec(executor)
            .await?;
    }
    Ok(())
}

fn to_i64(id: u64) -> Result<i64> {
    i64::try_from(id).context("migration id exceeds SQLite INTEGER")
}

pub(super) async fn inspect(request: SchemaInspectRequest<'_>) -> Result<ObservedSchema> {
    let namespace = request.namespace.unwrap_or("main").to_owned();
    let schema_ident = quote_identifier(&namespace);
    let rows = toasty::sql::query(format!(
        "SELECT name, COALESCE(sql, '') FROM {schema_ident}.sqlite_schema \
         WHERE type = 'table' ORDER BY name"
    ))
    .exec(request.db)
    .await?;
    let mut tables = BTreeMap::<String, ObservedTable>::new();
    let mut diagnostics = Vec::new();
    for row in rows {
        let values = record(&row, 2, "SQLite table catalog")?;
        let table_name = value_string(&values[0])?;
        if is_internal_table(&table_name) || !is_managed(&table_name, &request) {
            continue;
        }
        let create_sql = value_string(&values[1])?;
        let upper_sql = create_sql.to_ascii_uppercase();
        if upper_sql.contains("WITHOUT ROWID") {
            diagnostics.push(format!("sqlite_without_rowid:{table_name}"));
        }
        if upper_sql.contains(" STRICT") {
            diagnostics.push(format!("sqlite_strict_table:{table_name}"));
        }
        let table_ident = quote_identifier(&table_name);
        let column_rows =
            toasty::sql::query(format!("PRAGMA {schema_ident}.table_xinfo({table_ident})"))
                .exec(request.db)
                .await?;
        let mut columns = Vec::with_capacity(column_rows.len());
        let mut primary_columns = Vec::new();
        for row in column_rows {
            let values = record(&row, 7, "SQLite table_xinfo")?;
            let ordinal = usize::try_from(value_i64(&values[0])?)?;
            let name = value_string(&values[1])?;
            let native_type = value_string(&values[2])?;
            let primary_ordinal = value_i64(&values[5])?;
            let hidden = value_i64(&values[6])?;
            if hidden != 0 {
                diagnostics.push(format!("sqlite_generated_column:{table_name}.{name}"));
            }
            if primary_ordinal > 0 {
                primary_columns.push((primary_ordinal, name.clone()));
            }
            columns.push(ObservedColumn {
                name,
                data_type: sqlite_affinity(&native_type).to_owned(),
                native_type: native_type.clone(),
                nullable: value_i64(&values[3])? == 0,
                auto_increment: primary_ordinal > 0
                    && native_type.eq_ignore_ascii_case("INTEGER")
                    && upper_sql.contains("AUTOINCREMENT"),
                default: value_optional_string(&values[4])?,
                ordinal,
                comment: None,
            });
        }
        primary_columns.sort_by_key(|(ordinal, _)| *ordinal);
        let mut indices = Vec::new();
        if !primary_columns.is_empty() {
            indices.push(ObservedIndex {
                name: "PRIMARY".to_owned(),
                columns: primary_columns.into_iter().map(|(_, name)| name).collect(),
                unique: true,
                primary_key: true,
            });
        }
        let index_rows =
            toasty::sql::query(format!("PRAGMA {schema_ident}.index_list({table_ident})"))
                .exec(request.db)
                .await?;
        for row in index_rows {
            let values = record(&row, 5, "SQLite index_list")?;
            let index_name = value_string(&values[1])?;
            let unique = value_i64(&values[2])? != 0;
            let origin = value_string(&values[3])?;
            let partial = value_i64(&values[4])? != 0;
            if origin == "pk" {
                continue;
            }
            if partial {
                diagnostics.push(format!("sqlite_partial_index:{table_name}.{index_name}"));
            }
            let index_ident = quote_identifier(&index_name);
            let part_rows =
                toasty::sql::query(format!("PRAGMA {schema_ident}.index_xinfo({index_ident})"))
                    .exec(request.db)
                    .await?;
            let mut parts = Vec::new();
            for part in part_rows {
                let values = record(&part, 6, "SQLite index_xinfo")?;
                if value_i64(&values[5])? == 0 {
                    continue;
                }
                let cid = value_i64(&values[1])?;
                let Some(name) = value_optional_string(&values[2])? else {
                    diagnostics.push(format!("sqlite_expression_index:{table_name}.{index_name}"));
                    continue;
                };
                if cid < 0 {
                    diagnostics.push(format!("sqlite_expression_index:{table_name}.{index_name}"));
                    continue;
                }
                parts.push((value_i64(&values[0])?, name));
            }
            parts.sort_by_key(|(ordinal, _)| *ordinal);
            indices.push(ObservedIndex {
                name: index_name,
                columns: parts.into_iter().map(|(_, name)| name).collect(),
                unique,
                primary_key: false,
            });
        }
        let foreign_keys = toasty::sql::query(format!(
            "PRAGMA {schema_ident}.foreign_key_list({table_ident})"
        ))
        .exec(request.db)
        .await?;
        if !foreign_keys.is_empty() {
            diagnostics.push(format!("sqlite_foreign_key:{table_name}"));
        }
        tables.insert(
            table_name.clone(),
            ObservedTable {
                name: table_name,
                comment: None,
                columns,
                indices,
            },
        );
    }
    Ok(ObservedSchema {
        namespace,
        tables: tables.into_values().collect(),
        diagnostics,
    })
}

pub(super) fn normalize(observed: &ObservedSchema, target: &db::Schema) -> Result<db::Schema> {
    normalize_observed(observed, target, |column, target| {
        let inferred = sqlite_type(column)?;
        Ok(target
            .filter(|target| sqlite_storage_equivalent(column, &target.storage_ty))
            .map(|target| (target.ty.clone(), target.storage_ty.clone()))
            .unwrap_or(inferred))
    })
}

fn sqlite_type(column: &ObservedColumn) -> Result<(stmt::Type, db::Type)> {
    let native = column.native_type.trim();
    let upper = native.to_ascii_uppercase();
    if matches!(upper.as_str(), "BOOL" | "BOOLEAN") {
        return Ok((stmt::Type::Bool, db::Type::Boolean));
    }
    if upper == "JSON" {
        return Ok((stmt::Type::Object, db::Type::Json));
    }
    if upper == "JSONB" {
        return Ok((stmt::Type::Object, db::Type::Jsonb));
    }
    if let Some(size) = parse_sized_type(&upper, "VARCHAR") {
        return Ok((stmt::Type::String, db::Type::VarChar(size)));
    }
    Ok(match sqlite_affinity(native) {
        "integer" => (stmt::Type::I64, db::Type::Integer(8)),
        "real" => (stmt::Type::F64, db::Type::Float(8)),
        "text" => (stmt::Type::String, db::Type::Text),
        "blob" => (stmt::Type::Bytes, db::Type::Blob),
        "numeric" => (
            stmt::Type::Decimal,
            db::Type::Custom(if native.is_empty() {
                "NUMERIC".to_owned()
            } else {
                native.to_owned()
            }),
        ),
        other => bail!("unsupported_sqlite_column_type: {} ({other})", column.name),
    })
}

fn sqlite_storage_equivalent(column: &ObservedColumn, target: &db::Type) -> bool {
    let native = column.native_type.trim().to_ascii_uppercase();
    match target {
        db::Type::Boolean => matches!(native.as_str(), "BOOL" | "BOOLEAN"),
        db::Type::Integer(_) | db::Type::UnsignedInteger(_) => {
            sqlite_affinity(&native) == "integer"
        }
        db::Type::Float(_) => sqlite_affinity(&native) == "real",
        db::Type::Text => sqlite_affinity(&native) == "text",
        db::Type::VarChar(size) => parse_sized_type(&native, "VARCHAR") == Some(*size),
        db::Type::Blob | db::Type::Uuid | db::Type::Binary(_) => sqlite_affinity(&native) == "blob",
        db::Type::Document { .. } | db::Type::List(_) => sqlite_affinity(&native) == "text",
        db::Type::Json => native == "JSON",
        db::Type::Jsonb => native == "JSONB",
        db::Type::Numeric(_) => sqlite_affinity(&native) == "numeric",
        db::Type::Timestamp(_)
        | db::Type::Date
        | db::Type::Time(_)
        | db::Type::DateTime(_)
        | db::Type::Enum(_)
        | db::Type::Custom(_) => false,
    }
}

fn sqlite_affinity(native: &str) -> &'static str {
    let upper = native.to_ascii_uppercase();
    if upper.contains("INT") {
        "integer"
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        "text"
    } else if upper.contains("BLOB") || upper.trim().is_empty() {
        "blob"
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        "real"
    } else {
        "numeric"
    }
}

fn parse_sized_type(native: &str, name: &str) -> Option<u64> {
    native
        .strip_prefix(name)?
        .trim()
        .strip_prefix('(')?
        .strip_suffix(')')?
        .trim()
        .parse()
        .ok()
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
    name.starts_with("sqlite_") || matches!(name, "__toasty_migrations" | LEDGER)
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
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
        Value::U64(value) => i64::try_from(*value).context("catalog integer exceeds i64"),
        other => bail!("expected integer catalog value, got {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use toasty::schema::db;

    use super::{CREATE_LEDGER_SQL, sqlite_affinity, sqlite_storage_equivalent};
    use crate::migration::ObservedColumn;

    fn column(native_type: &str) -> ObservedColumn {
        ObservedColumn {
            name: "value".to_owned(),
            data_type: sqlite_affinity(native_type).to_owned(),
            native_type: native_type.to_owned(),
            nullable: false,
            auto_increment: false,
            default: None,
            ordinal: 0,
            comment: None,
        }
    }

    #[test]
    fn sqlite_affinity_and_target_equivalence_are_deterministic() {
        assert_eq!(sqlite_affinity("BIGINT"), "integer");
        assert_eq!(sqlite_affinity("VARCHAR(120)"), "text");
        assert_eq!(sqlite_affinity("DOUBLE"), "real");
        assert!(sqlite_storage_equivalent(
            &column("TEXT"),
            &db::Type::Document { binary: true }
        ));
        assert!(sqlite_storage_equivalent(
            &column("VARCHAR(120)"),
            &db::Type::VarChar(120)
        ));
        assert!(!sqlite_storage_equivalent(
            &column("VARCHAR(120)"),
            &db::Type::VarChar(191)
        ));
    }

    #[test]
    fn sqlite_ledger_is_source_scoped_without_foreign_keys() {
        assert!(CREATE_LEDGER_SQL.contains("PRIMARY KEY (source_code, id)"));
        assert!(!CREATE_LEDGER_SQL.contains("REFERENCES"));
    }
}
