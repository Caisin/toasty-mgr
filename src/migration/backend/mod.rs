use std::{future::Future, pin::Pin};

use anyhow::Result;
use toasty::{Db, schema::db};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedColumn {
    pub name: String,
    pub data_type: String,
    pub native_type: String,
    pub nullable: bool,
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
    fn inspect<'a>(&'a self, request: SchemaInspectRequest<'a>) -> InspectFuture<'a>;
    fn normalize(&self, observed: &ObservedSchema) -> Result<db::Schema>;
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
