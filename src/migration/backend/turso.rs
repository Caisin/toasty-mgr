use anyhow::Result;
use toasty::{Db, schema::db};

use super::{
    AppliedIdsFuture, ApplyFuture, BackendId, BackendMigration, DdlAtomicity, InspectFuture,
    LedgerMigration, MigrationBackend, ObservedSchema, PrepareLedgerFuture, RollbackFuture,
    SchemaInspectRequest, SchemaSyncFuture, sqlite,
};

/// Turso uses SQLite catalog and DDL semantics, but remains a separately registered backend so
/// driver-specific transaction behavior is covered independently and can diverge later.
pub struct TursoMigrationBackend {
    id: BackendId,
}

impl Default for TursoMigrationBackend {
    fn default() -> Self {
        Self {
            id: BackendId("turso".to_owned()),
        }
    }
}

impl MigrationBackend for TursoMigrationBackend {
    fn backend_id(&self) -> &BackendId {
        &self.id
    }

    fn aliases(&self) -> &[&str] {
        &["turso"]
    }

    fn ddl_atomicity(&self) -> DdlAtomicity {
        DdlAtomicity::Transactional
    }

    fn inspect<'a>(&'a self, request: SchemaInspectRequest<'a>) -> InspectFuture<'a> {
        Box::pin(async move { sqlite::inspect(request).await })
    }

    fn normalize(&self, observed: &ObservedSchema, target: &db::Schema) -> Result<db::Schema> {
        sqlite::normalize(observed, target)
    }

    fn sync_schema<'a>(
        &'a self,
        _source_code: &'a str,
        sql: String,
        db: &'a mut Db,
    ) -> SchemaSyncFuture<'a> {
        Box::pin(async move { sqlite::sync_schema(&sql, db).await })
    }

    fn inspect_applied_ids<'a>(
        &'a self,
        source_code: &'a str,
        tracked: &'a [LedgerMigration],
        db: &'a mut Db,
    ) -> AppliedIdsFuture<'a> {
        Box::pin(async move { sqlite::inspect_applied_ids(source_code, tracked, db).await })
    }

    fn prepare_ledger<'a>(
        &'a self,
        source_code: &'a str,
        tracked: &'a [LedgerMigration],
        db: &'a mut Db,
    ) -> PrepareLedgerFuture<'a> {
        Box::pin(async move { sqlite::prepare_ledger(source_code, tracked, db).await })
    }

    fn apply_migration<'a>(
        &'a self,
        migration: BackendMigration,
        db: &'a mut Db,
    ) -> ApplyFuture<'a> {
        Box::pin(async move { sqlite::apply_migration(migration, true, db).await })
    }

    fn record_migration<'a>(
        &'a self,
        migration: BackendMigration,
        db: &'a mut Db,
    ) -> ApplyFuture<'a> {
        Box::pin(async move { sqlite::apply_migration(migration, false, db).await })
    }

    fn rollback_migration<'a>(
        &'a self,
        migration: BackendMigration,
        db: &'a mut Db,
    ) -> RollbackFuture<'a> {
        Box::pin(async move { sqlite::rollback_migration(migration, db).await })
    }
}
