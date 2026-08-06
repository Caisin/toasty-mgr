use std::{future::Future, pin::Pin};

use anyhow::{Context, Result};
use toasty::{
    Db,
    schema::db::{
        self, Column, ColumnId, Index, IndexColumn, IndexId, IndexOp, IndexScope, PrimaryKey,
        Table, TableId,
    },
    stmt,
};

use super::SchemaScope;

mod mysql;
mod postgresql;
mod sqlite;
mod turso;

pub use mysql::MySqlMigrationBackend;
pub use postgresql::PostgreSqlMigrationBackend;
pub use sqlite::SqliteMigrationBackend;
pub use turso::TursoMigrationBackend;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BackendId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdlAtomicity {
    Transactional,
    ImplicitCommit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedColumn {
    pub name: String,
    pub data_type: String,
    pub native_type: String,
    pub nullable: bool,
    pub auto_increment: bool,
    pub default: Option<String>,
    pub ordinal: usize,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedIndex {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary_key: bool,
    /// Backend-specific filter for a partial index; Toasty's schema cannot represent it.
    pub predicate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedTable {
    pub name: String,
    pub comment: Option<String>,
    pub columns: Vec<ObservedColumn>,
    pub indices: Vec<ObservedIndex>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservedSchema {
    pub namespace: String,
    pub tables: Vec<ObservedTable>,
    pub diagnostics: Vec<String>,
}

impl ObservedSchema {
    pub fn fingerprint(&self) -> String {
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in format!("{self:?}").bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("fnv1a64:{hash:016x}")
    }
}

pub struct SchemaInspectRequest<'a> {
    pub source_code: &'a str,
    pub namespace: Option<&'a str>,
    pub scope: &'a SchemaScope,
    pub managed_tables: &'a [String],
    pub db: &'a mut Db,
}

pub type InspectFuture<'a> = Pin<Box<dyn Future<Output = Result<ObservedSchema>> + Send + 'a>>;
pub type AppliedIdsFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<u64>>> + Send + 'a>>;
pub type PrepareLedgerFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<u64>>> + Send + 'a>>;
pub type ApplyFuture<'a> = Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>;
pub type RollbackFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
pub type SchemaSyncFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerMigration {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendMigration {
    pub source_code: String,
    pub id: u64,
    pub name: String,
    pub sql: String,
}

/// Backend-specific catalog reader and schema normalizer.
pub trait MigrationBackend: Send + Sync + 'static {
    fn backend_id(&self) -> &BackendId;
    fn aliases(&self) -> &[&str];
    fn ddl_atomicity(&self) -> DdlAtomicity;
    fn inspect<'a>(&'a self, request: SchemaInspectRequest<'a>) -> InspectFuture<'a>;
    fn normalize(&self, observed: &ObservedSchema, target: &db::Schema) -> Result<db::Schema>;
    fn sync_schema<'a>(
        &'a self,
        source_code: &'a str,
        sql: String,
        db: &'a mut Db,
    ) -> SchemaSyncFuture<'a>;
    fn inspect_applied_ids<'a>(
        &'a self,
        source_code: &'a str,
        tracked: &'a [LedgerMigration],
        db: &'a mut Db,
    ) -> AppliedIdsFuture<'a>;
    fn prepare_ledger<'a>(
        &'a self,
        source_code: &'a str,
        tracked: &'a [LedgerMigration],
        db: &'a mut Db,
    ) -> PrepareLedgerFuture<'a>;
    fn apply_migration<'a>(
        &'a self,
        migration: BackendMigration,
        db: &'a mut Db,
    ) -> ApplyFuture<'a>;
    fn record_migration<'a>(
        &'a self,
        migration: BackendMigration,
        db: &'a mut Db,
    ) -> ApplyFuture<'a>;
    fn rollback_migration<'a>(
        &'a self,
        migration: BackendMigration,
        db: &'a mut Db,
    ) -> RollbackFuture<'a>;
}

pub(super) fn normalize_observed<F>(
    observed: &ObservedSchema,
    target: &db::Schema,
    mut column_type: F,
) -> Result<db::Schema>
where
    F: FnMut(&ObservedColumn, Option<&Column>) -> Result<(stmt::Type, db::Type)>,
{
    if let Some(diagnostic) = observed.diagnostics.first() {
        anyhow::bail!("migration_schema_unsupported: {diagnostic}");
    }
    let mut tables = Vec::with_capacity(observed.tables.len());
    for (table_index, observed_table) in observed.tables.iter().enumerate() {
        let table_id = TableId(table_index);
        let target_table = target
            .tables
            .iter()
            .find(|table| table.name == observed_table.name);
        let mut columns = Vec::with_capacity(observed_table.columns.len());
        for (column_index, observed_column) in observed_table.columns.iter().enumerate() {
            let target_column = target_table.and_then(|table| {
                table
                    .columns
                    .iter()
                    .find(|column| column.name == observed_column.name)
            });
            let (ty, storage_ty) = column_type(observed_column, target_column)?;
            columns.push(Column {
                id: ColumnId {
                    table: table_id,
                    index: column_index,
                },
                name: observed_column.name.clone(),
                ty,
                storage_ty,
                nullable: observed_column.nullable,
                primary_key: false,
                auto_increment: observed_column.auto_increment,
                versionable: target_column.is_some_and(|column| column.versionable),
            });
        }
        let mut indices = Vec::with_capacity(observed_table.indices.len());
        for observed_index in observed_table
            .indices
            .iter()
            .filter(|index| index.predicate.is_none())
        {
            let target_index = target_table.and_then(|table| {
                table
                    .indices
                    .iter()
                    .find(|candidate| index_shape_matches(table, candidate, observed_index))
            });
            // A physical auxiliary index can use Toasty's logical primary-index name while the
            // real PostgreSQL primary index has a backend-generated name. Once the real primary
            // matches by shape, keeping the auxiliary index would create two normalized indices
            // with one name and make schema sync oscillate forever.
            if target_index.is_none()
                && target_table.is_some_and(|table| {
                    table.indices.iter().any(|candidate| {
                        candidate.name == observed_index.name
                            && observed_table
                                .indices
                                .iter()
                                .any(|other| index_shape_matches(table, candidate, other))
                    })
                })
            {
                continue;
            }
            let columns_for_index = observed_index
                .columns
                .iter()
                .enumerate()
                .map(|(position, name)| {
                    let column_index = columns
                        .iter()
                        .position(|column| &column.name == name)
                        .with_context(|| {
                            format!(
                                "index {} references unknown column {name}",
                                observed_index.name
                            )
                        })?;
                    if observed_index.primary_key {
                        columns[column_index].primary_key = target_table
                            .and_then(|table| {
                                table.columns.iter().find(|column| column.name == *name)
                            })
                            .map(|column| column.primary_key)
                            .unwrap_or(true);
                    }
                    let (op, scope) = target_index
                        .and_then(|index| index.columns.get(position))
                        .map(|column| (column.op, column.scope))
                        .unwrap_or((IndexOp::Eq, IndexScope::Local));
                    Ok(IndexColumn {
                        column: columns[column_index].id,
                        op,
                        scope,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            indices.push(Index {
                id: IndexId {
                    table: table_id,
                    index: indices.len(),
                },
                name: target_index
                    .map(|index| index.name.clone())
                    .unwrap_or_else(|| observed_index.name.clone()),
                on: table_id,
                columns: columns_for_index,
                unique: observed_index.unique,
                primary_key: observed_index.primary_key,
            });
        }
        let primary_index = indices
            .iter()
            .find(|index| index.primary_key)
            .with_context(|| {
                format!("observed table has no primary key: {}", observed_table.name)
            })?;
        tables.push(Table {
            id: table_id,
            name: observed_table.name.clone(),
            columns,
            primary_key: PrimaryKey {
                columns: primary_index
                    .columns
                    .iter()
                    .map(|column| column.column)
                    .collect(),
                index: primary_index.id,
            },
            indices,
        });
    }
    Ok(db::Schema { tables })
}

fn index_shape_matches(table: &Table, index: &Index, observed: &ObservedIndex) -> bool {
    index.primary_key == observed.primary_key
        && index.unique == observed.unique
        && index.columns.len() == observed.columns.len()
        && index
            .columns
            .iter()
            .zip(&observed.columns)
            .all(|(column, name)| table.column(column.column).name == *name)
}

#[cfg(test)]
mod tests {
    use toasty::{schema::db, stmt};

    use super::{ObservedColumn, ObservedIndex, ObservedSchema, ObservedTable, normalize_observed};

    #[test]
    fn partial_indices_stay_in_fingerprint_but_not_in_toasty_schema() {
        let observed = ObservedSchema {
            namespace: "public".to_owned(),
            tables: vec![ObservedTable {
                name: "demo".to_owned(),
                comment: None,
                columns: vec![ObservedColumn {
                    name: "id".to_owned(),
                    data_type: "bigint".to_owned(),
                    native_type: "bigint".to_owned(),
                    nullable: false,
                    auto_increment: true,
                    default: None,
                    ordinal: 1,
                    comment: None,
                }],
                indices: vec![
                    ObservedIndex {
                        name: "demo_pkey".to_owned(),
                        columns: vec!["id".to_owned()],
                        unique: true,
                        primary_key: true,
                        predicate: None,
                    },
                    ObservedIndex {
                        name: "demo_id_present".to_owned(),
                        columns: vec!["id".to_owned()],
                        unique: true,
                        primary_key: false,
                        predicate: Some("id > 0".to_owned()),
                    },
                ],
            }],
            diagnostics: Vec::new(),
        };
        let fingerprint = observed.fingerprint();
        let normalized = normalize_observed(&observed, &db::Schema::default(), |_, _| {
            Ok((stmt::Type::I64, db::Type::Integer(8)))
        })
        .unwrap();

        assert_eq!(normalized.tables[0].indices.len(), 1);
        assert!(normalized.tables[0].indices[0].primary_key);
        assert!(fingerprint.contains("fnv1a64:"));
    }

    #[test]
    fn physical_primary_index_wins_over_shadowing_auxiliary_index() {
        let target_observed = schema_with_indices(vec![ObservedIndex {
            name: "logical_demo_pkey".to_owned(),
            columns: vec!["id".to_owned()],
            unique: true,
            primary_key: true,
            predicate: None,
        }]);
        let target = normalize_i64(&target_observed, &db::Schema::default());
        let live = schema_with_indices(vec![
            ObservedIndex {
                name: "demo_pkey".to_owned(),
                columns: vec!["id".to_owned()],
                unique: true,
                primary_key: true,
                predicate: None,
            },
            ObservedIndex {
                name: "logical_demo_pkey".to_owned(),
                columns: vec!["id".to_owned()],
                unique: true,
                primary_key: false,
                predicate: None,
            },
        ]);

        let normalized = normalize_i64(&live, &target);

        assert_eq!(normalized.tables[0].indices.len(), 1);
        assert_eq!(normalized.tables[0].indices[0].name, "logical_demo_pkey");
        assert!(normalized.tables[0].indices[0].primary_key);
    }

    fn schema_with_indices(indices: Vec<ObservedIndex>) -> ObservedSchema {
        ObservedSchema {
            namespace: "public".to_owned(),
            tables: vec![ObservedTable {
                name: "demo".to_owned(),
                comment: None,
                columns: vec![ObservedColumn {
                    name: "id".to_owned(),
                    data_type: "bigint".to_owned(),
                    native_type: "bigint".to_owned(),
                    nullable: false,
                    auto_increment: true,
                    default: None,
                    ordinal: 1,
                    comment: None,
                }],
                indices,
            }],
            diagnostics: Vec::new(),
        }
    }

    fn normalize_i64(observed: &ObservedSchema, target: &db::Schema) -> db::Schema {
        normalize_observed(observed, target, |_, _| {
            Ok((stmt::Type::I64, db::Type::Integer(8)))
        })
        .unwrap()
    }
}
