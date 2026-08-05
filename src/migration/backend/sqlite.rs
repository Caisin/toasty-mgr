use anyhow::{Result, bail};
use toasty::{Db, schema::db};

use super::{
    AppliedIdsFuture, ApplyFuture, BackendId, BackendMigration, InspectFuture, LedgerMigration,
    MigrationBackend, ObservedSchema, PrepareLedgerFuture, RollbackFuture, SchemaInspectRequest,
};

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
    fn inspect<'a>(&'a self, _request: SchemaInspectRequest<'a>) -> InspectFuture<'a> {
        Box::pin(async { bail!("migration_backend_introspection_unsupported: sqlite") })
    }
    fn normalize(&self, _observed: &ObservedSchema) -> Result<db::Schema> {
        bail!("migration_backend_normalization_unsupported: sqlite")
    }
    fn inspect_applied_ids<'a>(
        &'a self,
        _source_code: &'a str,
        _tracked: &'a [LedgerMigration],
        _db: &'a mut Db,
    ) -> AppliedIdsFuture<'a> {
        Box::pin(async { bail!("migration_ledger_inspection_unsupported: sqlite") })
    }
    fn prepare_ledger<'a>(
        &'a self,
        _: &'a str,
        _: &'a [LedgerMigration],
        _: &'a mut Db,
    ) -> PrepareLedgerFuture<'a> {
        Box::pin(async { bail!("migration_ledger_prepare_unsupported: sqlite") })
    }
    fn apply_migration<'a>(&'a self, _: BackendMigration, _: &'a mut Db) -> ApplyFuture<'a> {
        Box::pin(async { bail!("migration_apply_unsupported: sqlite") })
    }
    fn record_migration<'a>(&'a self, _: BackendMigration, _: &'a mut Db) -> ApplyFuture<'a> {
        Box::pin(async { bail!("migration_apply_unsupported: sqlite") })
    }
    fn rollback_migration<'a>(&'a self, _: BackendMigration, _: &'a mut Db) -> RollbackFuture<'a> {
        Box::pin(async { bail!("migration_rollback_unsupported: sqlite") })
    }
}
