use std::{collections::HashSet, fs, path::Path};

use anyhow::{Context, Result, bail};
use toasty::migration::{History, MigrationSet, Snapshot};

/// A migration SQL file with owned contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedMigrationFile {
    pub id: u64,
    pub name: String,
    pub sql: String,
    pub rollback_sql: Option<String>,
    pub snapshot_name: Option<String>,
}

/// A validated ordered set of migration files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationArtifactSet {
    migrations: Vec<OwnedMigrationFile>,
}

/// Filesystem lineage state used to prevent accidental partial regeneration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactState {
    Missing,
    Empty,
    Ready,
    Partial,
    Invalid,
}

impl MigrationArtifactSet {
    pub fn new(migrations: Vec<OwnedMigrationFile>) -> Result<Self> {
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for migration in &migrations {
            if !ids.insert(migration.id) {
                bail!("duplicate migration id: {}", migration.id);
            }
            if !names.insert(migration.name.as_str()) {
                bail!("duplicate migration file name: {}", migration.name);
            }
            if migration.sql.trim().is_empty() {
                bail!("empty migration SQL: {}", migration.name);
            }
        }
        Ok(Self { migrations })
    }

    pub fn from_embedded(set: MigrationSet) -> Result<Self> {
        Self::new(
            set.migrations()
                .iter()
                .map(|migration| OwnedMigrationFile {
                    id: migration.id(),
                    name: migration.name().to_owned(),
                    sql: migration.sql().to_owned(),
                    rollback_sql: None,
                    snapshot_name: None,
                })
                .collect(),
        )
    }

    pub fn load(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let history_path = root.join("history.toml");
        let history = History::load_or_default(&history_path)
            .with_context(|| format!("failed to load {}", history_path.display()))?;
        let migrations_dir = root.join("migrations");
        let rollbacks_dir = root.join("rollbacks");
        Self::from_history(&migrations_dir, &rollbacks_dir, &history)
    }

    pub fn from_history(
        migrations_dir: &Path,
        rollbacks_dir: &Path,
        history: &History,
    ) -> Result<Self> {
        let mut files = Vec::with_capacity(history.entries().len());
        for entry in history.entries() {
            let path = migrations_dir.join(&entry.name);
            let sql = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let rollback_path = rollbacks_dir.join(&entry.name);
            let rollback_sql = rollback_path
                .is_file()
                .then(|| fs::read_to_string(&rollback_path))
                .transpose()
                .with_context(|| format!("failed to read {}", rollback_path.display()))?;
            files.push(OwnedMigrationFile {
                id: entry.id,
                name: entry.name.clone(),
                sql,
                rollback_sql,
                snapshot_name: Some(entry.snapshot_name.clone()),
            });
        }
        Self::new(files)
    }

    pub fn migrations(&self) -> &[OwnedMigrationFile] {
        &self.migrations
    }

    pub fn len(&self) -> usize {
        self.migrations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.migrations.is_empty()
    }
}

pub(crate) fn inspect_state(root: &Path) -> ArtifactState {
    if !root.exists() {
        return ArtifactState::Missing;
    }
    let history_path = root.join("history.toml");
    let migrations_dir = root.join("migrations");
    let snapshots_dir = root.join("snapshots");
    let rollbacks_dir = root.join("rollbacks");
    let has_migrations = has_files(&migrations_dir);
    let has_snapshots = has_files(&snapshots_dir);
    if !history_path.exists() {
        return if has_migrations || has_snapshots {
            ArtifactState::Partial
        } else {
            ArtifactState::Empty
        };
    }
    let Ok(history) = History::load(&history_path) else {
        return ArtifactState::Invalid;
    };
    if history.entries().is_empty() {
        return if has_migrations || has_snapshots {
            ArtifactState::Partial
        } else {
            ArtifactState::Empty
        };
    }
    let migration_names = history
        .entries()
        .iter()
        .map(|entry| entry.name.clone())
        .collect::<HashSet<_>>();
    let snapshot_names = history
        .entries()
        .iter()
        .map(|entry| entry.snapshot_name.clone())
        .collect::<HashSet<_>>();
    let rollback_names = file_names(&rollbacks_dir, "sql");
    if file_names(&migrations_dir, "sql") != migration_names
        || file_names(&snapshots_dir, "toml") != snapshot_names
        || !rollback_names.is_subset(&migration_names)
    {
        return ArtifactState::Partial;
    }
    for entry in history.entries() {
        if !migrations_dir.join(&entry.name).is_file()
            || !snapshots_dir.join(&entry.snapshot_name).is_file()
        {
            return ArtifactState::Partial;
        }
        if load_snapshot(snapshots_dir.join(&entry.snapshot_name)).is_err() {
            return ArtifactState::Invalid;
        }
    }
    ArtifactState::Ready
}

fn file_names(path: &Path, extension: &str) -> HashSet<String> {
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|value| value.to_str()) == Some(extension))
                .then(|| entry.file_name().to_string_lossy().into_owned())
        })
        .collect()
}

fn has_files(path: &Path) -> bool {
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| entry.path().is_file())
}

pub(crate) fn load_snapshot(path: impl AsRef<Path>) -> Result<Snapshot> {
    let path = path.as_ref();
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    // Keep reviewed legacy artifacts immutable while accepting Toasty's old spelling.
    Ok(contents
        .replace("storage_ty = \"Bool\"", "storage_ty = \"Boolean\"")
        .parse()?)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{ArtifactState, MigrationArtifactSet, OwnedMigrationFile, inspect_state};
    use toasty::migration::{History, HistoryEntry};

    #[test]
    fn owned_artifacts_reject_duplicate_ids_names_and_empty_sql() {
        let file = |id, name: &str, sql: &str| OwnedMigrationFile {
            id,
            name: name.to_owned(),
            sql: sql.to_owned(),
            rollback_sql: None,
            snapshot_name: None,
        };
        assert!(MigrationArtifactSet::new(vec![file(1, "a.sql", "SELECT 1")]).is_ok());
        assert!(
            MigrationArtifactSet::new(vec![
                file(1, "a.sql", "SELECT 1"),
                file(1, "b.sql", "SELECT 2")
            ])
            .is_err()
        );
        assert!(
            MigrationArtifactSet::new(vec![
                file(1, "a.sql", "SELECT 1"),
                file(2, "a.sql", "SELECT 2")
            ])
            .is_err()
        );
        assert!(MigrationArtifactSet::new(vec![file(1, "a.sql", "  ")]).is_err());
    }

    #[test]
    fn artifact_state_allows_missing_and_empty_roots_but_rejects_orphans() {
        let root = std::env::temp_dir().join(format!(
            "toasty-mgr-artifact-state-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert_eq!(inspect_state(&root), ArtifactState::Missing);
        fs::create_dir_all(root.join("migrations")).unwrap();
        assert_eq!(inspect_state(&root), ArtifactState::Empty);
        fs::write(root.join("migrations/orphan.sql"), "SELECT 1").unwrap();
        assert_eq!(inspect_state(&root), ArtifactState::Partial);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn filesystem_artifacts_load_optional_matching_rollback_sql() {
        let root = std::env::temp_dir().join(format!(
            "toasty-mgr-rollback-artifacts-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("migrations")).unwrap();
        fs::create_dir_all(root.join("rollbacks")).unwrap();
        let mut history = History::new();
        history.add_entry(HistoryEntry {
            id: 1,
            name: "auth_1_demo.sql".to_owned(),
            snapshot_name: "auth_1_demo.toml".to_owned(),
            checksum: None,
        });
        fs::write(
            root.join("migrations/auth_1_demo.sql"),
            "CREATE TABLE demo(id BIGINT);",
        )
        .unwrap();
        fs::write(root.join("rollbacks/auth_1_demo.sql"), "DROP TABLE demo;").unwrap();

        let artifacts = MigrationArtifactSet::from_history(
            &root.join("migrations"),
            &root.join("rollbacks"),
            &history,
        )
        .unwrap();
        assert_eq!(
            artifacts.migrations()[0].rollback_sql.as_deref(),
            Some("DROP TABLE demo;")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
