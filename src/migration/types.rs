use std::{collections::BTreeSet, path::PathBuf};

use toasty::migration::MigrationSet;

use super::{ArtifactState, MigrationArtifactSet};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MigrationGroupKey(pub String);

impl From<&str> for MigrationGroupKey {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SchemaScope {
    #[default]
    Managed,
    Tables(BTreeSet<String>),
    Prefixes(Vec<String>),
    NamespaceExclusive,
}

#[derive(Debug, Clone)]
pub struct MigrationSourceConfig {
    pub code: String,
    pub artifact_root: PathBuf,
    pub migration_group: MigrationGroupKey,
    pub backend_override: Option<String>,
    pub namespace: Option<String>,
    pub scope: SchemaScope,
}

#[derive(Debug, Clone)]
pub struct MigrationSourcesConfig {
    pub artifact_root: PathBuf,
    pub migration_group: MigrationGroupKey,
    pub backend_override: Option<String>,
    pub namespace: Option<String>,
    pub scope: SchemaScope,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SchemaOrigin {
    #[default]
    Auto,
    LatestSnapshot,
    LiveDatabase,
    Empty,
}

#[derive(Debug, Clone, Default)]
pub struct MigrationGenerateRequest {
    pub source: String,
    pub name: String,
    pub origin: SchemaOrigin,
    pub rename_tables: Vec<String>,
    pub rename_columns: Vec<String>,
    pub rename_indices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationGenerateOutcome {
    pub source: String,
    pub created: bool,
    pub id: Option<u64>,
    pub migration_path: Option<PathBuf>,
    pub rollback_path: Option<PathBuf>,
    pub snapshot_path: Option<PathBuf>,
    pub artifact_state: ArtifactState,
}

#[derive(Debug, Clone)]
pub enum MigrationArtifactInput {
    RegisteredFilesystem,
    Owned(MigrationArtifactSet),
    Embedded(MigrationSet),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MigrationApplyMode {
    #[default]
    Execute,
    AdoptBaseline,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MigrationApplyReport {
    pub applied: usize,
    pub skipped: usize,
    pub adopted: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationRollbackSelection {
    Steps(usize),
    Target(u64),
}

impl Default for MigrationRollbackSelection {
    fn default() -> Self {
        Self::Steps(1)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MigrationRollbackReport {
    pub rolled_back: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationCheckReport {
    pub sources: usize,
    pub migrations: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationStatusReport {
    pub source: String,
    pub tracked: usize,
    pub applied: usize,
    pub pending: Vec<u64>,
    pub unknown_applied: Vec<u64>,
    pub model_drift: bool,
    pub database_fingerprint: Option<String>,
}
