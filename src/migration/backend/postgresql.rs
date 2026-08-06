use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result, bail};
use toasty::{
    Db, Executor,
    schema::db,
    stmt::{self, Value},
};

use super::{
    AppliedIdsFuture, ApplyFuture, BackendId, BackendMigration, DdlAtomicity, InspectFuture,
    LedgerMigration, MigrationBackend, ObservedColumn, ObservedIndex, ObservedSchema,
    ObservedTable, PrepareLedgerFuture, RollbackFuture, SchemaInspectRequest, SchemaSyncFuture,
    normalize_observed,
};
use crate::migration::SchemaScope;

pub struct PostgreSqlMigrationBackend {
    id: BackendId,
}

impl Default for PostgreSqlMigrationBackend {
    fn default() -> Self {
        Self {
            id: BackendId("postgresql".to_owned()),
        }
    }
}

impl MigrationBackend for PostgreSqlMigrationBackend {
    fn backend_id(&self) -> &BackendId {
        &self.id
    }

    fn aliases(&self) -> &[&str] {
        &["postgres", "postgresql"]
    }

    fn ddl_atomicity(&self) -> DdlAtomicity {
        DdlAtomicity::Transactional
    }

    fn inspect<'a>(&'a self, request: SchemaInspectRequest<'a>) -> InspectFuture<'a> {
        Box::pin(async move { inspect(request).await })
    }

    fn normalize(&self, observed: &ObservedSchema, target: &db::Schema) -> Result<db::Schema> {
        normalize_observed(observed, target, |column, target| {
            let inferred = postgres_type(column)?;
            Ok(target
                .filter(|target| postgres_storage_equivalent(&inferred.1, &target.storage_ty))
                .map(|target| (target.ty.clone(), target.storage_ty.clone()))
                .unwrap_or(inferred))
        })
    }

    fn sync_schema<'a>(
        &'a self,
        source_code: &'a str,
        sql: String,
        db: &'a mut Db,
    ) -> SchemaSyncFuture<'a> {
        Box::pin(async move { sync_schema(source_code, &sql, db).await })
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
    source_code TEXT NOT NULL, id BIGINT NOT NULL CHECK (id >= 0), name TEXT NOT NULL, \
    applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, \
    PRIMARY KEY (source_code, id))";

async fn inspect_applied_ids(
    source_code: &str,
    tracked: &[LedgerMigration],
    db: &mut Db,
) -> Result<Vec<u64>> {
    let tracked = tracked_by_id(tracked);
    let mut applied = HashSet::new();
    if table_exists(db, LEDGER).await? {
        for (id, name) in composite_rows(source_code, db).await? {
            if let Some(expected) = tracked.get(&id)
                && expected != &name
            {
                bail!(
                    "migration ledger name mismatch for id {id}: expected {expected}, found {name}"
                );
            }
            applied.insert(id);
        }
    }
    let mut applied = applied.into_iter().collect::<Vec<_>>();
    applied.sort_unstable();
    Ok(applied)
}

async fn prepare_ledger(
    source_code: &str,
    tracked: &[LedgerMigration],
    db: &mut Db,
) -> Result<Vec<u64>> {
    let tracked = tracked_by_id(tracked);
    let mut tx = db.transaction().await?;
    create_ledger(&mut tx).await?;
    lock_source(source_code, &mut tx).await?;
    let rows = composite_rows(source_code, &mut tx).await?;
    let mut ids = Vec::with_capacity(rows.len());
    for (id, name) in rows {
        validate_tracked_name(&tracked, id, &name, "migration ledger")?;
        ids.push(id);
    }
    tx.commit().await?;
    Ok(ids)
}

async fn apply_migration(
    migration: BackendMigration,
    execute_sql: bool,
    db: &mut Db,
) -> Result<bool> {
    let mut tx = db.transaction().await?;
    create_ledger(&mut tx).await?;
    lock_source(&migration.source_code, &mut tx).await?;
    let existing = toasty::sql::query(
        "SELECT name::text FROM __toasty_mgr_migrations WHERE source_code = $1 AND id = $2",
    )
    .bind(migration.source_code.clone())
    .bind(to_i64(migration.id)?)
    .exec(&mut tx)
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
        tx.commit().await?;
        return Ok(false);
    }
    if execute_sql {
        execute_sql_statements(&migration.sql, &mut tx).await?;
    }
    toasty::sql::statement(
        "INSERT INTO __toasty_mgr_migrations (source_code, id, name) VALUES ($1, $2, $3)",
    )
    .bind(migration.source_code)
    .bind(to_i64(migration.id)?)
    .bind(migration.name)
    .exec(&mut tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

async fn sync_schema(source_code: &str, sql: &str, db: &mut Db) -> Result<()> {
    let mut tx = db.transaction().await?;
    lock_source(source_code, &mut tx).await?;
    execute_sql_statements(sql, &mut tx).await?;
    tx.commit().await?;
    Ok(())
}

async fn rollback_migration(migration: BackendMigration, db: &mut Db) -> Result<()> {
    let mut tx = db.transaction().await?;
    create_ledger(&mut tx).await?;
    lock_source(&migration.source_code, &mut tx).await?;
    let rows = toasty::sql::query(
        "SELECT id::bigint, name::text FROM __toasty_mgr_migrations \
         WHERE source_code = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(migration.source_code.clone())
    .exec(&mut tx)
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
    execute_sql_statements(&migration.sql, &mut tx).await?;
    toasty::sql::statement(
        "DELETE FROM __toasty_mgr_migrations WHERE source_code = $1 AND id = $2",
    )
    .bind(migration.source_code)
    .bind(to_i64(migration.id)?)
    .exec(&mut tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn create_ledger(executor: &mut dyn Executor) -> Result<()> {
    toasty::sql::statement(CREATE_LEDGER_SQL)
        .exec(executor)
        .await?;
    Ok(())
}

async fn lock_source(source_code: &str, executor: &mut dyn Executor) -> Result<()> {
    toasty::sql::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))::text")
        .bind(format!("toasty-mgr:{source_code}"))
        .exec(executor)
        .await?;
    Ok(())
}

async fn table_exists(executor: &mut dyn Executor, table: &str) -> Result<bool> {
    let rows = toasty::sql::query("SELECT to_regclass($1)::text")
        .bind(table.to_owned())
        .exec(executor)
        .await?;
    Ok(rows
        .first()
        .and_then(first_value)
        .is_some_and(|value| !value_is_null(value)))
}

async fn composite_rows(
    source_code: &str,
    executor: &mut dyn Executor,
) -> Result<Vec<(u64, String)>> {
    toasty::sql::query(
        "SELECT id::bigint, name::text FROM __toasty_mgr_migrations \
         WHERE source_code = $1 ORDER BY id",
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
    for breakpoint_statement in migration.statements() {
        for statement in split_postgresql_statements(breakpoint_statement)? {
            toasty::sql::statement(statement).exec(executor).await?;
        }
    }
    Ok(())
}

#[derive(Debug)]
enum PostgreSqlLexState {
    Normal,
    SingleQuoted { backslash_escapes: bool },
    DoubleQuoted,
    DollarQuoted(Vec<u8>),
    LineComment,
    BlockComment(usize),
}

fn split_postgresql_statements(sql: &str) -> Result<Vec<String>> {
    let bytes = sql.as_bytes();
    let mut statements = Vec::new();
    let mut state = PostgreSqlLexState::Normal;
    let mut start = 0;
    let mut index = 0;
    let mut has_code = false;

    while index < bytes.len() {
        match &mut state {
            PostgreSqlLexState::Normal => match bytes[index] {
                b'-' if bytes.get(index + 1) == Some(&b'-') => {
                    state = PostgreSqlLexState::LineComment;
                    index += 2;
                }
                b'/' if bytes.get(index + 1) == Some(&b'*') => {
                    state = PostgreSqlLexState::BlockComment(1);
                    index += 2;
                }
                b'\'' => {
                    has_code = true;
                    state = PostgreSqlLexState::SingleQuoted {
                        backslash_escapes: quote_uses_backslash_escapes(bytes, index),
                    };
                    index += 1;
                }
                b'"' => {
                    has_code = true;
                    state = PostgreSqlLexState::DoubleQuoted;
                    index += 1;
                }
                b'$' => {
                    if let Some(delimiter) = dollar_quote_delimiter(bytes, index) {
                        has_code = true;
                        index += delimiter.len();
                        state = PostgreSqlLexState::DollarQuoted(delimiter.to_vec());
                    } else {
                        has_code = true;
                        index += 1;
                    }
                }
                b';' => {
                    if has_code {
                        statements.push(sql[start..index].trim().to_owned());
                    }
                    start = index + 1;
                    has_code = false;
                    index += 1;
                }
                byte => {
                    has_code |= !byte.is_ascii_whitespace();
                    index += 1;
                }
            },
            PostgreSqlLexState::SingleQuoted { backslash_escapes } => {
                if *backslash_escapes && bytes[index] == b'\\' {
                    index += usize::from(index + 1 < bytes.len()) + 1;
                } else if bytes[index] == b'\'' {
                    if bytes.get(index + 1) == Some(&b'\'') {
                        index += 2;
                    } else {
                        state = PostgreSqlLexState::Normal;
                        index += 1;
                    }
                } else {
                    index += 1;
                }
            }
            PostgreSqlLexState::DoubleQuoted => {
                if bytes[index] == b'"' {
                    if bytes.get(index + 1) == Some(&b'"') {
                        index += 2;
                    } else {
                        state = PostgreSqlLexState::Normal;
                        index += 1;
                    }
                } else {
                    index += 1;
                }
            }
            PostgreSqlLexState::DollarQuoted(delimiter) => {
                if bytes[index..].starts_with(delimiter) {
                    index += delimiter.len();
                    state = PostgreSqlLexState::Normal;
                } else {
                    index += 1;
                }
            }
            PostgreSqlLexState::LineComment => {
                if matches!(bytes[index], b'\n' | b'\r') {
                    state = PostgreSqlLexState::Normal;
                }
                index += 1;
            }
            PostgreSqlLexState::BlockComment(depth) => {
                if bytes[index..].starts_with(b"/*") {
                    *depth += 1;
                    index += 2;
                } else if bytes[index..].starts_with(b"*/") {
                    *depth -= 1;
                    index += 2;
                    if *depth == 0 {
                        state = PostgreSqlLexState::Normal;
                    }
                } else {
                    index += 1;
                }
            }
        }
    }

    match state {
        PostgreSqlLexState::Normal | PostgreSqlLexState::LineComment => {}
        PostgreSqlLexState::SingleQuoted { .. } => {
            bail!("unterminated PostgreSQL single-quoted string in migration SQL")
        }
        PostgreSqlLexState::DoubleQuoted => {
            bail!("unterminated PostgreSQL quoted identifier in migration SQL")
        }
        PostgreSqlLexState::DollarQuoted(delimiter) => bail!(
            "unterminated PostgreSQL dollar-quoted string {} in migration SQL",
            String::from_utf8_lossy(&delimiter)
        ),
        PostgreSqlLexState::BlockComment(_) => {
            bail!("unterminated PostgreSQL block comment in migration SQL")
        }
    }

    if has_code {
        statements.push(sql[start..].trim().to_owned());
    }
    Ok(statements)
}

fn quote_uses_backslash_escapes(bytes: &[u8], quote: usize) -> bool {
    let escaped = quote > 0
        && matches!(bytes[quote - 1], b'e' | b'E')
        && (quote == 1 || !is_identifier_continue(bytes[quote - 2]));
    let unicode = quote > 1
        && bytes[quote - 2..quote].eq_ignore_ascii_case(b"u&")
        && (quote == 2 || !is_identifier_continue(bytes[quote - 3]));
    escaped || unicode
}

fn dollar_quote_delimiter(bytes: &[u8], start: usize) -> Option<&[u8]> {
    let next = *bytes.get(start + 1)?;
    if next == b'$' {
        return Some(&bytes[start..=start + 1]);
    }
    if !is_identifier_start(next) {
        return None;
    }
    let mut index = start + 2;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'$' {
            return Some(&bytes[start..=index]);
        }
        if !is_identifier_continue(byte) {
            return None;
        }
        index += 1;
    }
    None
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || !byte.is_ascii()
}

fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit() || byte == b'$'
}

fn to_i64(id: u64) -> Result<i64> {
    i64::try_from(id).context("migration id exceeds PostgreSQL BIGINT")
}

async fn inspect(request: SchemaInspectRequest<'_>) -> Result<ObservedSchema> {
    let namespace = match request.namespace {
        Some(value) => value.to_owned(),
        None => {
            let rows = toasty::sql::query("SELECT current_schema()::text")
                .exec(request.db)
                .await?;
            value_string(
                rows.first()
                    .and_then(first_value)
                    .context("PostgreSQL returned no current schema")?,
            )?
        }
    };
    let column_rows = toasty::sql::query(
        "SELECT c.table_name::text, c.column_name::text, c.data_type::text, \
         pg_catalog.format_type(a.atttypid, a.atttypmod)::text, c.is_nullable::text, COALESCE(c.column_default, '')::text, \
         c.is_identity::text, c.ordinal_position::bigint, \
         COALESCE(pg_catalog.col_description(cl.oid, a.attnum), '')::text \
         FROM information_schema.columns c \
         JOIN pg_catalog.pg_class cl ON cl.relname = c.table_name \
         JOIN pg_catalog.pg_namespace n ON n.oid = cl.relnamespace AND n.nspname = c.table_schema \
         JOIN pg_catalog.pg_attribute a ON a.attrelid = cl.oid AND a.attname = c.column_name \
         WHERE c.table_schema = $1 AND cl.relkind = 'r' \
         ORDER BY c.table_name, c.ordinal_position",
    )
    .bind(namespace.clone())
    .exec(request.db)
    .await?;
    let index_rows = toasty::sql::query(
        "SELECT tbl.relname::text, idx.relname::text, ix.indisunique::text, \
         ix.indisprimary::text, string_agg(att.attname::text, ',' ORDER BY ord.n)::text, \
         COALESCE(pg_catalog.pg_get_expr(ix.indpred, ix.indrelid), '')::text \
         FROM pg_catalog.pg_index ix \
         JOIN pg_catalog.pg_class tbl ON tbl.oid = ix.indrelid \
         JOIN pg_catalog.pg_namespace ns ON ns.oid = tbl.relnamespace \
         JOIN pg_catalog.pg_class idx ON idx.oid = ix.indexrelid \
         JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY ord(attnum, n) ON true \
         JOIN pg_catalog.pg_attribute att ON att.attrelid = tbl.oid AND att.attnum = ord.attnum \
         WHERE ns.nspname = $1 AND tbl.relkind = 'r' \
         GROUP BY tbl.relname, idx.relname, ix.indisunique, ix.indisprimary, ix.indpred, ix.indrelid \
         ORDER BY tbl.relname, idx.relname",
    )
    .bind(namespace.clone())
    .exec(request.db)
    .await?;
    let managed = |name: &str| match request.scope {
        SchemaScope::Managed => request.managed_tables.iter().any(|table| table == name),
        SchemaScope::Tables(tables) => tables.contains(name),
        SchemaScope::Prefixes(prefixes) => prefixes.iter().any(|prefix| name.starts_with(prefix)),
        SchemaScope::NamespaceExclusive => true,
    };
    let mut tables = BTreeMap::<String, ObservedTable>::new();
    for row in column_rows {
        let Value::Record(row) = row else {
            bail!("PostgreSQL catalog returned a non-record column row");
        };
        let table_name = value_string(&row[0])?;
        if table_name == "__toasty_migrations" || !managed(&table_name) {
            continue;
        }
        let default = value_string(&row[5])?;
        tables
            .entry(table_name.clone())
            .or_insert_with(|| ObservedTable {
                name: table_name,
                comment: None,
                columns: Vec::new(),
                indices: Vec::new(),
            })
            .columns
            .push(ObservedColumn {
                name: value_string(&row[1])?,
                data_type: value_string(&row[2])?,
                native_type: value_string(&row[3])?,
                nullable: value_string(&row[4])? == "YES",
                auto_increment: postgres_auto_increment(&default, &value_string(&row[6])?),
                default: (!default.is_empty()).then_some(default),
                ordinal: usize::try_from(value_i64(&row[7])?)?,
                comment: nonempty(value_string(&row[8])?),
            });
    }
    for row in index_rows {
        let Value::Record(row) = row else {
            bail!("PostgreSQL catalog returned a non-record index row");
        };
        let table_name = value_string(&row[0])?;
        let Some(table) = tables.get_mut(&table_name) else {
            continue;
        };
        table.indices.push(ObservedIndex {
            name: value_string(&row[1])?,
            unique: value_string(&row[2])? == "true",
            primary_key: value_string(&row[3])? == "true",
            columns: value_string(&row[4])?
                .split(',')
                .map(str::to_owned)
                .collect(),
            predicate: nonempty(value_string(&row[5])?),
        });
    }
    Ok(ObservedSchema {
        namespace,
        tables: tables.into_values().collect(),
        diagnostics: Vec::new(),
    })
}

fn postgres_type(column: &ObservedColumn) -> Result<(stmt::Type, db::Type)> {
    Ok(match column.data_type.as_str() {
        "boolean" => (stmt::Type::Bool, db::Type::Boolean),
        "smallint" => (stmt::Type::I16, db::Type::Integer(2)),
        "integer" => (stmt::Type::I32, db::Type::Integer(4)),
        "bigint" => (stmt::Type::I64, db::Type::Integer(8)),
        "real" => (stmt::Type::F32, db::Type::Float(4)),
        "double precision" => (stmt::Type::F64, db::Type::Float(8)),
        "uuid" => (stmt::Type::Uuid, db::Type::Uuid),
        "bytea" => (stmt::Type::Bytes, db::Type::Blob),
        "numeric" => (stmt::Type::Decimal, db::Type::Numeric(None)),
        "date" => (stmt::Type::Date, db::Type::Date),
        "time without time zone" => (stmt::Type::Time, db::Type::Time(6)),
        "timestamp without time zone" => (stmt::Type::DateTime, db::Type::DateTime(6)),
        "timestamp with time zone" => (stmt::Type::Timestamp, db::Type::Timestamp(6)),
        "json" => (stmt::Type::Object, db::Type::Json),
        "jsonb" => (stmt::Type::Object, db::Type::Jsonb),
        "character varying" => (
            stmt::Type::String,
            parse_varchar(&column.native_type).unwrap_or(db::Type::Text),
        ),
        "text" => (stmt::Type::String, db::Type::Text),
        "ARRAY" => postgres_array_type(&column.native_type)?,
        "USER-DEFINED" => (
            stmt::Type::String,
            db::Type::Custom(column.native_type.clone()),
        ),
        other => bail!(
            "unsupported_postgresql_column_type: {} ({other}/{})",
            column.name,
            column.native_type
        ),
    })
}

fn postgres_storage_equivalent(inferred: &db::Type, target: &db::Type) -> bool {
    inferred == target
        || matches!(
            (inferred, target),
            (db::Type::Json, db::Type::Document { binary: false })
                | (db::Type::Jsonb, db::Type::Document { binary: true })
        )
        || matches!(
            (inferred, target),
            (db::Type::Integer(left), db::Type::UnsignedInteger(right)) if left == right
        )
}

fn postgres_auto_increment(default: &str, is_identity: &str) -> bool {
    is_identity == "YES" || default.starts_with("nextval(")
}

fn parse_varchar(native: &str) -> Option<db::Type> {
    native
        .strip_prefix("character varying(")?
        .strip_suffix(')')?
        .parse()
        .ok()
        .map(db::Type::VarChar)
}

fn postgres_array_type(native: &str) -> Result<(stmt::Type, db::Type)> {
    let elem = native
        .strip_suffix("[]")
        .context("PostgreSQL ARRAY type has no [] suffix")?;
    let (stmt_ty, storage_ty) = match elem {
        "text" | "character varying" => (stmt::Type::String, db::Type::Text),
        "smallint" => (stmt::Type::I16, db::Type::Integer(2)),
        "integer" => (stmt::Type::I32, db::Type::Integer(4)),
        "bigint" => (stmt::Type::I64, db::Type::Integer(8)),
        "boolean" => (stmt::Type::Bool, db::Type::Boolean),
        "uuid" => (stmt::Type::Uuid, db::Type::Uuid),
        other => bail!("unsupported_postgresql_array_type: {other}"),
    };
    Ok((
        stmt::Type::List(Box::new(stmt_ty)),
        db::Type::List(Box::new(storage_ty)),
    ))
}

fn first_value(row: &Value) -> Option<&Value> {
    match row {
        Value::Record(record) => record.first(),
        _ => None,
    }
}
fn value_is_null(value: &Value) -> bool {
    matches!(value, Value::Null)
}
fn value_string(value: &Value) -> Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Null => Ok(String::new()),
        other => bail!("expected string catalog value, got {other:?}"),
    }
}
fn value_i64(value: &Value) -> Result<i64> {
    match value {
        Value::I64(value) => Ok(*value),
        Value::I32(value) => Ok(i64::from(*value)),
        other => bail!("expected integer catalog value, got {other:?}"),
    }
}
fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
#[path = "postgresql_tests.rs"]
mod tests;
