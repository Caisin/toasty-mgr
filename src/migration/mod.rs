//! Multi-source schema migration authoring, validation, and execution.

mod artifact;
mod backend;
mod manager;
mod types;

pub use artifact::{ArtifactState, MigrationArtifactSet, OwnedMigrationFile};
pub use backend::{
    BackendId, BackendMigration, DdlAtomicity, LedgerMigration, MigrationBackend,
    MySqlMigrationBackend, ObservedColumn, ObservedIndex, ObservedSchema, ObservedTable,
    PostgreSqlMigrationBackend, SchemaInspectRequest, SqliteMigrationBackend,
    TursoMigrationBackend,
};
pub use manager::TcMigrationMgr;
pub use types::{
    MigrationApplyMode, MigrationApplyReport, MigrationArtifactInput, MigrationCheckReport,
    MigrationGenerateOutcome, MigrationGenerateRequest, MigrationGroupKey, MigrationRollbackReport,
    MigrationRollbackSelection, MigrationSourceConfig, MigrationSourcesConfig,
    MigrationStatusReport, MigrationSyncReport, SchemaOrigin, SchemaScope,
};
