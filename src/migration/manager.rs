use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, RwLock},
};

use anyhow::{Context, Result, bail};
use toasty::{
    Db,
    migration::{Generated, History, HistoryEntry},
    schema::{db, diff},
};

use crate::{TcMgr, registry::TcModelSets};

use super::{
    ArtifactState, BackendMigration, LedgerMigration, MigrationApplyMode, MigrationApplyReport,
    MigrationArtifactInput, MigrationArtifactSet, MigrationBackend, MigrationCheckReport,
    MigrationGenerateOutcome, MigrationGenerateRequest, MigrationRollbackReport,
    MigrationRollbackSelection, MigrationSourceConfig, MigrationSourcesConfig,
    MigrationStatusReport, MigrationSyncReport, MySqlMigrationBackend, PostgreSqlMigrationBackend,
    SchemaInspectRequest, SchemaOrigin, SchemaScope, SqliteMigrationBackend, TursoMigrationBackend,
    artifact::{inspect_state, load_snapshot},
};

static SOURCES: OnceLock<RwLock<HashMap<String, MigrationSourceConfig>>> = OnceLock::new();
static BACKENDS: OnceLock<RwLock<HashMap<String, Arc<dyn MigrationBackend>>>> = OnceLock::new();
static ARTIFACTS: OnceLock<RwLock<HashMap<String, MigrationArtifactInput>>> = OnceLock::new();

fn sources() -> &'static RwLock<HashMap<String, MigrationSourceConfig>> {
    SOURCES.get_or_init(|| RwLock::new(HashMap::new()))
}

fn backends() -> &'static RwLock<HashMap<String, Arc<dyn MigrationBackend>>> {
    BACKENDS.get_or_init(|| {
        let mut values = HashMap::new();
        for backend in [
            Arc::new(PostgreSqlMigrationBackend::default()) as Arc<dyn MigrationBackend>,
            Arc::new(MySqlMigrationBackend::default()),
            Arc::new(SqliteMigrationBackend::default()),
            Arc::new(TursoMigrationBackend::default()),
        ] {
            values.insert(backend.backend_id().0.clone(), backend.clone());
            for alias in backend.aliases() {
                values.insert((*alias).to_owned(), backend.clone());
            }
        }
        RwLock::new(values)
    })
}

fn registered_artifacts() -> &'static RwLock<HashMap<String, MigrationArtifactInput>> {
    ARTIFACTS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Process-wide migration manager for explicitly registered sources.
pub struct TcMigrationMgr;

impl TcMigrationMgr {
    pub fn register_model_sources(config: MigrationSourcesConfig) -> Result<Vec<String>> {
        let codes = TcModelSets::entries_with_base()
            .into_iter()
            .map(|(code, _)| code)
            .collect::<Vec<_>>();
        for code in &codes {
            insert_source(MigrationSourceConfig {
                code: code.clone(),
                artifact_root: config.artifact_root.join(code),
                migration_group: config.migration_group.clone(),
                backend_override: config.backend_override.clone(),
                namespace: config.namespace.clone(),
                scope: config.scope.clone(),
            });
        }
        Ok(codes)
    }

    pub fn register_source(config: MigrationSourceConfig) -> Result<()> {
        TcMgr::models(&config.code)
            .with_context(|| format!("migration source has no models: {}", config.code))?;
        insert_source(config);
        Ok(())
    }

    pub fn set_registered_artifacts(source: &str, artifacts: MigrationArtifactInput) -> Result<()> {
        source_config(source)?;
        write(registered_artifacts()).insert(source.to_owned(), artifacts);
        Ok(())
    }

    pub fn register_backend(backend: Arc<dyn MigrationBackend>) -> Result<()> {
        let mut values = write(backends());
        let mut names = vec![backend.backend_id().0.clone()];
        names.extend(backend.aliases().iter().map(|value| (*value).to_owned()));
        for name in &names {
            if values.contains_key(name) {
                bail!("migration backend already registered: {name}");
            }
        }
        for name in names {
            values.insert(name, backend.clone());
        }
        Ok(())
    }

    pub fn source_codes() -> Vec<String> {
        let mut values = read(sources()).keys().cloned().collect::<Vec<_>>();
        values
            .sort_by(|left, right| (left != crate::BASE, left).cmp(&(right != crate::BASE, right)));
        values
    }

    pub fn artifact_state(source: &str) -> Result<ArtifactState> {
        let config = source_config(source)?;
        Ok(inspect_state(&config.artifact_root))
    }

    pub async fn generate_all(name: impl Into<String>) -> Result<Vec<MigrationGenerateOutcome>> {
        let name = name.into();
        let mut outcomes = Vec::with_capacity(Self::source_codes().len());
        for source in Self::source_codes() {
            outcomes.push(
                Self::generate(MigrationGenerateRequest {
                    source,
                    name: name.clone(),
                    ..MigrationGenerateRequest::default()
                })
                .await?,
            );
        }
        Ok(outcomes)
    }

    pub async fn sync(source: &str, dry_run: bool) -> Result<MigrationSyncReport> {
        let config = source_config(source)?;
        let mut db = TcMgr::get(&config.code).await?;
        let backend = backend_for(&config, &db)?;
        let observed = inspect_normalized_schema(&config, &mut db, backend.as_ref()).await?;
        let current = db.schema().db.clone();
        let Some(generated) = toasty::migration::generate(
            db.driver(),
            &observed,
            &current,
            &diff::RenameHints::new(),
        ) else {
            return Ok(MigrationSyncReport {
                source: config.code,
                changed: false,
                sql: None,
            });
        };
        let db::Migration::Sql(sql) = generated.migration;
        validate_sql(&sql)?;
        if !dry_run {
            backend
                .sync_schema(&config.code, sql.clone(), &mut db)
                .await?;
            let synchronized =
                inspect_normalized_schema(&config, &mut db, backend.as_ref()).await?;
            if toasty::migration::generate(
                db.driver(),
                &current,
                &synchronized,
                &diff::RenameHints::new(),
            )
            .is_some()
            {
                bail!("migration_sync_incomplete: {}", config.code);
            }
        }
        Ok(MigrationSyncReport {
            source: config.code,
            changed: true,
            sql: Some(sql),
        })
    }

    pub async fn sync_all(dry_run: bool) -> Result<Vec<MigrationSyncReport>> {
        let mut outcomes = Vec::with_capacity(Self::source_codes().len());
        for source in Self::source_codes() {
            outcomes.push(Self::sync(&source, dry_run).await?);
        }
        Ok(outcomes)
    }

    pub async fn apply_all() -> Result<MigrationApplyReport> {
        let codes = Self::source_codes();
        for source in &codes {
            let status = Self::status(source, false).await?;
            if !status.unknown_applied.is_empty() {
                bail!(
                    "unknown applied migration ids for source {}: {:?}",
                    source,
                    status.unknown_applied
                );
            }
        }
        let mut summary = MigrationApplyReport::default();
        for source in codes {
            let report = Self::apply(&source, MigrationApplyMode::Execute).await?;
            summary.applied += report.applied;
            summary.skipped += report.skipped;
            summary.adopted += report.adopted;
        }
        Ok(summary)
    }

    pub async fn status_all(inspect_database: bool) -> Result<Vec<MigrationStatusReport>> {
        let mut reports = Vec::with_capacity(Self::source_codes().len());
        for source in Self::source_codes() {
            reports.push(Self::status(&source, inspect_database).await?);
        }
        Ok(reports)
    }

    pub async fn generate(request: MigrationGenerateRequest) -> Result<MigrationGenerateOutcome> {
        Self::generate_internal(request).await
    }

    async fn generate_internal(
        request: MigrationGenerateRequest,
    ) -> Result<MigrationGenerateOutcome> {
        let config = source_config(&request.source)?;
        let state = inspect_state(&config.artifact_root);
        if matches!(state, ArtifactState::Partial | ArtifactState::Invalid) {
            bail!(
                "migration_artifacts_not_regenerable: {} ({state:?})",
                config.code
            );
        }
        let history_path = config.artifact_root.join("history.toml");
        let snapshots_dir = config.artifact_root.join("snapshots");
        let mut history = History::load_or_default(&history_path)?;
        let mut db = TcMgr::get(&config.code).await?;
        let current_schema = db.schema().db.clone();
        let mut baseline_written = None;
        let previous_schema = match request.origin {
            SchemaOrigin::Auto => match state {
                ArtifactState::Missing | ArtifactState::Empty => db::Schema::default(),
                ArtifactState::Ready => latest_schema(&history, &snapshots_dir)?,
                ArtifactState::Partial | ArtifactState::Invalid => unreachable!(),
            },
            SchemaOrigin::Empty => {
                if !matches!(state, ArtifactState::Missing | ArtifactState::Empty) {
                    bail!("migration Empty origin requires an empty lineage");
                }
                db::Schema::default()
            }
            SchemaOrigin::LatestSnapshot => {
                if state != ArtifactState::Ready {
                    bail!("latest snapshot origin requires a ready lineage");
                }
                latest_schema(&history, &snapshots_dir)?
            }
            SchemaOrigin::LiveDatabase => {
                let backend = backend_for(&config, &db)?;
                let managed_tables = managed_table_names(&current_schema);
                let observed = backend
                    .inspect(SchemaInspectRequest {
                        source_code: &config.code,
                        namespace: config.namespace.as_deref(),
                        scope: &config.scope,
                        managed_tables: &managed_tables,
                        db: &mut db,
                    })
                    .await?;
                let mut observed_schema = backend.normalize(&observed, &current_schema)?;
                if state == ArtifactState::Ready {
                    let latest = latest_schema(&history, &snapshots_dir)?;
                    observed_schema =
                        project_managed_indices(observed_schema, &latest, &config.scope);
                    if let Some(drift) = toasty::migration::generate(
                        db.driver(),
                        &latest,
                        &observed_schema,
                        &diff::RenameHints::new(),
                    ) {
                        bail!("tracked_schema_drift: {drift:?}");
                    }
                } else if !observed_schema.tables.is_empty() {
                    let baseline = toasty::migration::generate(
                        db.driver(),
                        &db::Schema::default(),
                        &observed_schema,
                        &diff::RenameHints::new(),
                    )
                    .context("database schema produced no replayable baseline")?;
                    let rollback = toasty::migration::generate(
                        db.driver(),
                        &observed_schema,
                        &db::Schema::default(),
                        &diff::RenameHints::new(),
                    )
                    .context("database schema produced no rollback migration")?;
                    baseline_written = Some(write_generated(
                        &config,
                        &mut history,
                        "observed_schema_baseline",
                        baseline,
                        rollback,
                    )?);
                }
                observed_schema
            }
        };
        let hints = build_rename_hints(
            &previous_schema,
            &current_schema,
            &request.rename_tables,
            &request.rename_columns,
            &request.rename_indices,
        )?;
        let Some(generated) =
            toasty::migration::generate(db.driver(), &previous_schema, &current_schema, &hints)
        else {
            if let Some((id, migration_path, rollback_path, snapshot_path)) = baseline_written {
                return Ok(MigrationGenerateOutcome {
                    source: config.code,
                    created: true,
                    id: Some(id),
                    migration_path: Some(migration_path),
                    rollback_path: Some(rollback_path),
                    snapshot_path: Some(snapshot_path),
                    artifact_state: ArtifactState::Ready,
                });
            }
            return Ok(MigrationGenerateOutcome {
                source: config.code,
                created: false,
                id: None,
                migration_path: None,
                rollback_path: None,
                snapshot_path: None,
                artifact_state: state,
            });
        };
        let rollback = toasty::migration::generate(
            db.driver(),
            &current_schema,
            &previous_schema,
            &diff::RenameHints::new(),
        )
        .context("model schema produced no rollback migration")?;
        let (id, migration_path, rollback_path, snapshot_path) =
            write_generated(&config, &mut history, &request.name, generated, rollback)?;
        Ok(MigrationGenerateOutcome {
            source: config.code,
            created: true,
            id: Some(id),
            migration_path: Some(migration_path),
            rollback_path: Some(rollback_path),
            snapshot_path: Some(snapshot_path),
            artifact_state: ArtifactState::Ready,
        })
    }

    /// Generates a replayable `Empty -> ObservedSchema` baseline without changing the database.
    pub async fn baseline(source: &str, name: &str) -> Result<MigrationGenerateOutcome> {
        let config = source_config(source)?;
        let state = inspect_state(&config.artifact_root);
        if !matches!(state, ArtifactState::Missing | ArtifactState::Empty) {
            bail!(
                "migration baseline requires an empty lineage: {} ({state:?})",
                config.code
            );
        }
        let mut db = TcMgr::get(&config.code).await?;
        let backend = backend_for(&config, &db)?;
        let managed_tables = managed_table_names(&db.schema().db);
        let observed = backend
            .inspect(SchemaInspectRequest {
                source_code: &config.code,
                namespace: config.namespace.as_deref(),
                scope: &config.scope,
                managed_tables: &managed_tables,
                db: &mut db,
            })
            .await?;
        let observed_schema = backend.normalize(&observed, &db.schema().db)?;
        if observed_schema.tables.is_empty() {
            bail!("migration baseline requires an existing observed schema");
        }
        let generated = toasty::migration::generate(
            db.driver(),
            &db::Schema::default(),
            &observed_schema,
            &diff::RenameHints::new(),
        )
        .context("database schema produced no replayable baseline")?;
        let rollback = toasty::migration::generate(
            db.driver(),
            &observed_schema,
            &db::Schema::default(),
            &diff::RenameHints::new(),
        )
        .context("database schema produced no rollback migration")?;
        let mut history = History::new();
        let (id, migration_path, rollback_path, snapshot_path) =
            write_generated(&config, &mut history, name, generated, rollback)?;
        Ok(MigrationGenerateOutcome {
            source: config.code,
            created: true,
            id: Some(id),
            migration_path: Some(migration_path),
            rollback_path: Some(rollback_path),
            snapshot_path: Some(snapshot_path),
            artifact_state: ArtifactState::Ready,
        })
    }

    pub async fn apply(source: &str, mode: MigrationApplyMode) -> Result<MigrationApplyReport> {
        let config = source_config(source)?;
        let artifact_input = registered_artifact_input(source)?;
        let baseline_schema = if mode == MigrationApplyMode::AdoptBaseline {
            match &artifact_input {
                MigrationArtifactInput::RegisteredFilesystem => {
                    Some(first_snapshot_schema(&config)?)
                }
                MigrationArtifactInput::Owned(_) | MigrationArtifactInput::Embedded(_) => {
                    bail!("migration baseline adoption requires registered filesystem artifacts")
                }
            }
        } else {
            None
        };
        let artifacts = resolve_artifacts(&config, artifact_input)?;
        if baseline_schema.is_some() && artifacts.is_empty() {
            bail!("migration baseline adoption requires at least one tracked migration");
        }
        let mut db = TcMgr::get(&config.code).await?;
        if let Some(baseline_schema) = baseline_schema.as_ref() {
            let backend = backend_for(&config, &db)?;
            let managed_tables = managed_table_names(&db.schema().db);
            let observed = backend
                .inspect(SchemaInspectRequest {
                    source_code: &config.code,
                    namespace: config.namespace.as_deref(),
                    scope: &config.scope,
                    managed_tables: &managed_tables,
                    db: &mut db,
                })
                .await?;
            let observed_schema = backend.normalize(&observed, &db.schema().db)?;
            if toasty::migration::generate(
                db.driver(),
                baseline_schema,
                &observed_schema,
                &diff::RenameHints::new(),
            )
            .is_some()
            {
                bail!("migration_baseline_database_drift: {}", config.code);
            }
        }
        let backend = backend_for(&config, &db)?;
        let tracked = ledger_migrations(&artifacts);
        let inspected = backend
            .inspect_applied_ids(&config.code, &tracked, &mut db)
            .await?;
        validate_applied_ids(&artifacts, &inspected)?;
        let mut applied_ids = backend
            .prepare_ledger(&config.code, &tracked, &mut db)
            .await?
            .into_iter()
            .collect::<HashSet<_>>();
        validate_applied_ids(&artifacts, &applied_ids.iter().copied().collect::<Vec<_>>())?;
        let mut report = MigrationApplyReport::default();
        let adopted_id = if mode == MigrationApplyMode::AdoptBaseline {
            if !applied_ids.is_empty() {
                bail!(
                    "migration baseline adoption requires no applied IDs for source {}",
                    config.code
                );
            }
            let baseline = artifacts
                .migrations()
                .first()
                .context("migration baseline adoption requires a first migration")?;
            backend
                .record_migration(
                    backend_migration(&config.code, baseline, String::new()),
                    &mut db,
                )
                .await?;
            applied_ids.insert(baseline.id);
            report.adopted = 1;
            Some(baseline.id)
        } else {
            None
        };
        for migration in artifacts.migrations() {
            if adopted_id == Some(migration.id) {
                continue;
            }
            if applied_ids.contains(&migration.id) {
                report.skipped += 1;
                continue;
            }
            validate_sql(&migration.sql)?;
            let applied = backend
                .apply_migration(
                    backend_migration(&config.code, migration, migration.sql.clone()),
                    &mut db,
                )
                .await?;
            applied_ids.insert(migration.id);
            if applied {
                report.applied += 1;
            } else {
                report.skipped += 1;
            }
        }
        Ok(report)
    }

    pub async fn rollback(
        source: &str,
        selection: MigrationRollbackSelection,
    ) -> Result<MigrationRollbackReport> {
        let config = source_config(source)?;
        let artifacts = resolve_artifacts(&config, registered_artifact_input(source)?)?;
        let mut db = TcMgr::get(&config.code).await?;
        let backend = backend_for(&config, &db)?;
        let tracked = ledger_migrations(&artifacts);
        let applied = backend
            .inspect_applied_ids(&config.code, &tracked, &mut db)
            .await?;
        validate_applied_ids(&artifacts, &applied)?;
        backend
            .prepare_ledger(&config.code, &tracked, &mut db)
            .await?;
        let applied = applied_lineage(&artifacts, &applied)?;
        let selected = select_rollback_ids(&applied, selection)?;
        let migrations = selected
            .iter()
            .map(|id| {
                let migration = artifacts
                    .migrations()
                    .iter()
                    .find(|migration| migration.id == *id)
                    .with_context(|| format!("rollback migration is not tracked: {id}"))?;
                let sql = migration.rollback_sql.clone().with_context(|| {
                    format!("migration rollback artifact missing: {}", migration.name)
                })?;
                validate_sql(&sql)?;
                Ok(backend_migration(&config.code, migration, sql))
            })
            .collect::<Result<Vec<_>>>()?;
        for migration in migrations {
            backend.rollback_migration(migration, &mut db).await?;
        }
        Ok(MigrationRollbackReport {
            rolled_back: selected.len(),
        })
    }

    pub async fn check(source: Option<&str>) -> Result<MigrationCheckReport> {
        let codes = match source {
            Some(source) => vec![source.to_owned()],
            None => Self::source_codes(),
        };
        let mut report = MigrationCheckReport::default();
        for code in codes {
            let config = source_config(&code)?;
            if inspect_state(&config.artifact_root) != ArtifactState::Ready {
                bail!("migration artifacts are not ready: {code}");
            }
            let artifacts = MigrationArtifactSet::load(&config.artifact_root)?;
            for migration in artifacts.migrations() {
                validate_sql(&migration.sql)?;
                if let Some(sql) = &migration.rollback_sql {
                    validate_sql(sql)?;
                }
            }
            let history = History::load(config.artifact_root.join("history.toml"))?;
            let previous = latest_schema(&history, &config.artifact_root.join("snapshots"))?;
            let db = TcMgr::get(&code).await?;
            if toasty::migration::generate(
                db.driver(),
                &previous,
                &db.schema().db,
                &diff::RenameHints::new(),
            )
            .is_some()
            {
                bail!("{code} model schema differs from latest snapshot");
            }
            report.sources += 1;
            report.migrations += artifacts.len();
        }
        Ok(report)
    }

    pub async fn status(source: &str, inspect_database: bool) -> Result<MigrationStatusReport> {
        let config = source_config(source)?;
        let artifacts = resolve_artifacts(&config, registered_artifact_input(source)?)?;
        let mut db = TcMgr::get(&config.code).await?;
        let backend = backend_for(&config, &db)?;
        let tracked = ledger_migrations(&artifacts);
        let applied_ids = backend
            .inspect_applied_ids(&config.code, &tracked, &mut db)
            .await?;
        let tracked_ids = artifacts
            .migrations()
            .iter()
            .map(|migration| migration.id)
            .collect::<HashSet<_>>();
        let applied_set = applied_ids.iter().copied().collect::<HashSet<_>>();
        let mut pending = tracked_ids
            .difference(&applied_set)
            .copied()
            .collect::<Vec<_>>();
        let mut unknown_applied = applied_set
            .difference(&tracked_ids)
            .copied()
            .collect::<Vec<_>>();
        pending.sort_unstable();
        unknown_applied.sort_unstable();
        let mut model_drift = false;
        if inspect_state(&config.artifact_root) == ArtifactState::Ready {
            let history = History::load(config.artifact_root.join("history.toml"))?;
            let latest = latest_schema(&history, &config.artifact_root.join("snapshots"))?;
            model_drift = toasty::migration::generate(
                db.driver(),
                &latest,
                &db.schema().db,
                &diff::RenameHints::new(),
            )
            .is_some();
        }
        let database_fingerprint = if inspect_database {
            let managed_tables = managed_table_names(&db.schema().db);
            Some(
                backend
                    .inspect(SchemaInspectRequest {
                        source_code: &config.code,
                        namespace: config.namespace.as_deref(),
                        scope: &config.scope,
                        managed_tables: &managed_tables,
                        db: &mut db,
                    })
                    .await?
                    .fingerprint(),
            )
        } else {
            None
        };
        Ok(MigrationStatusReport {
            source: config.code,
            tracked: artifacts.len(),
            applied: tracked_ids.intersection(&applied_set).count(),
            pending,
            unknown_applied,
            model_drift,
            database_fingerprint,
        })
    }
}

fn insert_source(config: MigrationSourceConfig) {
    write(registered_artifacts()).insert(
        config.code.clone(),
        MigrationArtifactInput::RegisteredFilesystem,
    );
    write(sources()).insert(config.code.clone(), config);
}

fn source_config(code: &str) -> Result<MigrationSourceConfig> {
    read(sources())
        .get(code)
        .cloned()
        .with_context(|| format!("migration_source_not_registered: {code}"))
}

fn registered_artifact_input(code: &str) -> Result<MigrationArtifactInput> {
    read(registered_artifacts())
        .get(code)
        .cloned()
        .with_context(|| format!("migration_artifacts_not_registered: {code}"))
}

fn backend_for(config: &MigrationSourceConfig, db: &Db) -> Result<Arc<dyn MigrationBackend>> {
    let id = config
        .backend_override
        .clone()
        .unwrap_or_else(|| db.capability().driver_name.to_ascii_lowercase());
    read(backends())
        .get(&id)
        .cloned()
        .with_context(|| format!("migration_backend_not_registered: {id}"))
}

async fn inspect_normalized_schema(
    config: &MigrationSourceConfig,
    db: &mut Db,
    backend: &dyn MigrationBackend,
) -> Result<db::Schema> {
    let managed_tables = managed_table_names(&db.schema().db);
    let observed = backend
        .inspect(SchemaInspectRequest {
            source_code: &config.code,
            namespace: config.namespace.as_deref(),
            scope: &config.scope,
            managed_tables: &managed_tables,
            db: &mut *db,
        })
        .await?;
    backend.normalize(&observed, &db.schema().db)
}

fn project_managed_indices(
    mut observed: db::Schema,
    target: &db::Schema,
    scope: &SchemaScope,
) -> db::Schema {
    if matches!(scope, SchemaScope::NamespaceExclusive) {
        return observed;
    }
    for observed_table in &mut observed.tables {
        let Some(target_table) = target
            .tables
            .iter()
            .find(|table| table.name == observed_table.name)
        else {
            continue;
        };
        let observed_columns = &observed_table.columns;
        observed_table.indices.retain(|observed_index| {
            observed_index.unique
                || target_table.indices.iter().any(|target_index| {
                    observed_index.unique == target_index.unique
                        && observed_index.primary_key == target_index.primary_key
                        && observed_index.columns.len() == target_index.columns.len()
                        && observed_index
                            .columns
                            .iter()
                            .zip(&target_index.columns)
                            .all(|(observed_column, target_column)| {
                                observed_columns[observed_column.column.index].name
                                    == target_table.columns[target_column.column.index].name
                            })
                })
        });
    }
    observed
}

fn resolve_artifacts(
    config: &MigrationSourceConfig,
    input: MigrationArtifactInput,
) -> Result<MigrationArtifactSet> {
    match input {
        MigrationArtifactInput::RegisteredFilesystem => {
            match inspect_state(&config.artifact_root) {
                ArtifactState::Ready => MigrationArtifactSet::load(&config.artifact_root),
                ArtifactState::Missing | ArtifactState::Empty => bail!(
                    "migration_artifacts_missing: source {}; run migration generate in a source checkout",
                    config.code
                ),
                ArtifactState::Partial | ArtifactState::Invalid => bail!(
                    "migration_artifacts_partial: source {}; restore the original migration history or remove the incomplete new lineage",
                    config.code
                ),
            }
        }
        MigrationArtifactInput::Owned(artifacts) => Ok(artifacts),
        MigrationArtifactInput::Embedded(set) => MigrationArtifactSet::from_embedded(set),
    }
}

fn validate_applied_ids(artifacts: &MigrationArtifactSet, applied: &[u64]) -> Result<()> {
    let tracked = artifacts
        .migrations()
        .iter()
        .map(|migration| migration.id)
        .collect::<HashSet<_>>();
    let unknown = applied
        .iter()
        .filter(|id| !tracked.contains(*id))
        .copied()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        bail!("unknown applied migration ids for source: {unknown:?}");
    }
    Ok(())
}

fn ledger_migrations(artifacts: &MigrationArtifactSet) -> Vec<LedgerMigration> {
    artifacts
        .migrations()
        .iter()
        .map(|migration| LedgerMigration {
            id: migration.id,
            name: migration.name.clone(),
        })
        .collect()
}

fn backend_migration(
    source_code: &str,
    migration: &super::OwnedMigrationFile,
    sql: String,
) -> BackendMigration {
    BackendMigration {
        source_code: source_code.to_owned(),
        id: migration.id,
        name: migration.name.clone(),
        sql,
    }
}

fn applied_lineage(artifacts: &MigrationArtifactSet, applied: &[u64]) -> Result<Vec<u64>> {
    let applied = applied.iter().copied().collect::<HashSet<_>>();
    let mut lineage = Vec::new();
    let mut pending_seen = false;
    for migration in artifacts.migrations() {
        if applied.contains(&migration.id) {
            if pending_seen {
                bail!(
                    "applied migration lineage contains a gap before {}",
                    migration.id
                );
            }
            lineage.push(migration.id);
        } else {
            pending_seen = true;
        }
    }
    Ok(lineage)
}

fn select_rollback_ids(applied: &[u64], selection: MigrationRollbackSelection) -> Result<Vec<u64>> {
    match selection {
        MigrationRollbackSelection::Steps(0) => bail!("migration rollback steps must be positive"),
        MigrationRollbackSelection::Steps(steps) => {
            if steps > applied.len() {
                bail!(
                    "migration rollback requested {steps} steps but only {} are applied",
                    applied.len()
                );
            }
            Ok(applied.iter().rev().take(steps).copied().collect())
        }
        MigrationRollbackSelection::Target(target) => {
            let index = applied
                .iter()
                .position(|id| *id == target)
                .with_context(|| format!("migration rollback target is not applied: {target}"))?;
            Ok(applied[index..].iter().rev().copied().collect())
        }
    }
}

fn first_snapshot_schema(config: &MigrationSourceConfig) -> Result<db::Schema> {
    let history = History::load(config.artifact_root.join("history.toml"))?;
    let first = history
        .entries()
        .first()
        .context("migration baseline adoption requires a first history entry")?;
    Ok(load_snapshot(
        config
            .artifact_root
            .join("snapshots")
            .join(&first.snapshot_name),
    )?
    .schema)
}

fn latest_schema(history: &History, snapshots_dir: &Path) -> Result<db::Schema> {
    let Some(entry) = history.entries().last() else {
        return Ok(db::Schema::default());
    };
    Ok(load_snapshot(snapshots_dir.join(&entry.snapshot_name))?.schema)
}

fn write_generated(
    config: &MigrationSourceConfig,
    history: &mut History,
    name: &str,
    generated: Generated,
    rollback: Generated,
) -> Result<(u64, PathBuf, PathBuf, PathBuf)> {
    let previous = history.entries().last().map(|entry| entry.id);
    let id = next_migration_id(previous)?;
    let slug = sanitize_name(name)?;
    let source = sanitize_name(&config.code)?;
    let migration_name = format!("{source}_{id}_{slug}.sql");
    let snapshot_name = format!("{source}_{id}_{slug}.toml");
    let migrations_dir = config.artifact_root.join("migrations");
    let rollbacks_dir = config.artifact_root.join("rollbacks");
    let snapshots_dir = config.artifact_root.join("snapshots");
    fs::create_dir_all(&migrations_dir)?;
    fs::create_dir_all(&rollbacks_dir)?;
    fs::create_dir_all(&snapshots_dir)?;
    let migration_path = migrations_dir.join(&migration_name);
    let rollback_path = rollbacks_dir.join(&migration_name);
    let snapshot_path = snapshots_dir.join(&snapshot_name);
    let migration_temp = migrations_dir.join(format!(".{migration_name}.tmp"));
    let rollback_temp = rollbacks_dir.join(format!(".{migration_name}.tmp"));
    let snapshot_temp = snapshots_dir.join(format!(".{snapshot_name}.tmp"));
    let history_path = config.artifact_root.join("history.toml");
    let history_temp = config.artifact_root.join(".history.toml.tmp");
    if migration_path.exists()
        || rollback_path.exists()
        || snapshot_path.exists()
        || migration_temp.exists()
        || rollback_temp.exists()
        || snapshot_temp.exists()
        || history_temp.exists()
    {
        bail!("refusing to overwrite migration artifact {id}_{slug}");
    }
    let Generated {
        migration,
        snapshot,
    } = generated;
    let db::Migration::Sql(sql) = migration;
    let db::Migration::Sql(rollback_sql) = rollback.migration;
    validate_sql(&sql)?;
    validate_sql(&rollback_sql)?;
    fs::write(&migration_temp, format!("{}\n", normalize_sql(&sql)))?;
    fs::write(
        &rollback_temp,
        format!("{}\n", normalize_sql(&rollback_sql)),
    )?;
    snapshot.save(&snapshot_temp)?;
    history.add_entry(HistoryEntry {
        id,
        name: migration_name,
        snapshot_name,
        checksum: None,
    });
    history.save(&history_temp)?;
    fs::rename(&migration_temp, &migration_path)?;
    fs::rename(&rollback_temp, &rollback_path)?;
    fs::rename(&snapshot_temp, &snapshot_path)?;
    fs::rename(&history_temp, &history_path)?;
    Ok((id, migration_path, rollback_path, snapshot_path))
}

fn sanitize_name(name: &str) -> Result<String> {
    let mut slug = String::new();
    let mut underscore = false;
    for ch in name.trim().chars() {
        let next = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '_'
        };
        if next == '_' {
            if !underscore {
                slug.push(next);
            }
            underscore = true;
        } else {
            slug.push(next);
            underscore = false;
        }
    }
    let slug = slug.trim_matches('_').to_owned();
    if slug.is_empty() {
        bail!("migration name must contain an ASCII letter or digit");
    }
    Ok(slug)
}

fn next_migration_id(previous: Option<u64>) -> Result<u64> {
    previous
        .map(|id| id.checked_add(1).context("migration id exhausted"))
        .transpose()
        .map(|id| id.unwrap_or(1))
}

fn normalize_sql(sql: &str) -> String {
    sql.replace("DROP INDEX \"", "DROP INDEX IF EXISTS \"")
}

fn validate_sql(sql: &str) -> Result<()> {
    if sql.trim().is_empty() {
        bail!("migration SQL must not be empty");
    }
    let upper = sql.to_ascii_uppercase();
    for forbidden in ["REFERENCES", " ON DELETE CASCADE", " ON UPDATE CASCADE"] {
        if upper.contains(forbidden) {
            bail!("migration SQL contains forbidden clause: {forbidden}");
        }
    }
    Ok(())
}

fn managed_table_names(schema: &db::Schema) -> Vec<String> {
    schema
        .tables
        .iter()
        .map(|table| table.name.clone())
        .collect()
}

fn build_rename_hints(
    previous: &db::Schema,
    next: &db::Schema,
    tables: &[String],
    columns: &[String],
    indices: &[String],
) -> Result<diff::RenameHints> {
    let mut hints = diff::RenameHints::new();
    for spec in tables {
        let (from, to) = split_spec(spec)?;
        hints.add_table_hint(table_id(previous, from)?, table_id(next, to)?);
    }
    for spec in columns {
        let (from, to) = split_spec(spec)?;
        hints.add_column_hint(column_id(previous, from)?, column_id(next, to)?);
    }
    for spec in indices {
        let (from, to) = split_spec(spec)?;
        hints.add_index_hint(index_id(previous, from)?, index_id(next, to)?);
    }
    Ok(hints)
}

fn split_spec(spec: &str) -> Result<(&str, &str)> {
    let (from, to) = spec
        .split_once('=')
        .with_context(|| format!("rename spec `{spec}` must use old=new"))?;
    if from.trim().is_empty() || to.trim().is_empty() {
        bail!("rename spec `{spec}` contains an empty side");
    }
    Ok((from.trim(), to.trim()))
}

fn split_path(path: &str) -> Result<(&str, &str)> {
    path.split_once('.')
        .with_context(|| format!("schema path `{path}` must use table.name"))
}

fn table_id(schema: &db::Schema, name: &str) -> Result<db::TableId> {
    schema
        .tables
        .iter()
        .find(|table| table.name == name)
        .map(|table| table.id)
        .with_context(|| format!("table `{name}` not found"))
}

fn column_id(schema: &db::Schema, path: &str) -> Result<db::ColumnId> {
    let (table_name, name) = split_path(path)?;
    let table = schema
        .tables
        .iter()
        .find(|table| table.name == table_name)
        .with_context(|| format!("table `{table_name}` not found"))?;
    table
        .columns
        .iter()
        .find(|column| column.name == name)
        .map(|column| column.id)
        .with_context(|| format!("column `{path}` not found"))
}

fn index_id(schema: &db::Schema, path: &str) -> Result<db::IndexId> {
    let (table_name, name) = split_path(path)?;
    let table = schema
        .tables
        .iter()
        .find(|table| table.name == table_name)
        .with_context(|| format!("table `{table_name}` not found"))?;
    table
        .indices
        .iter()
        .find(|index| index.name == name)
        .map(|index| index.id)
        .with_context(|| format!("index `{path}` not found"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        backends, next_migration_id, normalize_sql, read, resolve_artifacts, sanitize_name,
        select_rollback_ids, source_config, validate_sql,
    };
    use crate::migration::{
        MigrationArtifactInput, MigrationGroupKey, MigrationRollbackSelection,
        MigrationSourceConfig, MigrationSourcesConfig, SchemaScope, TcMigrationMgr,
    };
    use crate::{BaseDs, TcMgr};

    #[test]
    fn built_in_backend_registry_routes_to_each_database_adapter() {
        let values = read(backends());
        for (id, canonical) in [
            ("postgresql", "postgresql"),
            ("postgres", "postgresql"),
            ("mysql", "mysql"),
            ("sqlite", "sqlite"),
            ("turso", "turso"),
        ] {
            let backend = values
                .get(id)
                .unwrap_or_else(|| panic!("missing built-in migration backend {id}"));
            assert_eq!(backend.backend_id().0, canonical);
        }
    }

    #[test]
    fn filesystem_apply_rejects_a_missing_artifact_lineage() {
        let root = PathBuf::from(format!(
            "/tmp/toasty-mgr-missing-artifacts-{}",
            std::process::id()
        ));
        let config = MigrationSourceConfig {
            code: "missing-test-source".to_owned(),
            artifact_root: root,
            migration_group: MigrationGroupKey("missing-test-group".to_owned()),
            backend_override: Some("postgresql".to_owned()),
            namespace: None,
            scope: SchemaScope::Managed,
        };

        let error = resolve_artifacts(&config, MigrationArtifactInput::RegisteredFilesystem)
            .unwrap_err()
            .to_string();
        assert!(error.contains("migration_artifacts_missing"));
    }

    #[test]
    fn rollback_selection_is_descending_and_target_is_inclusive() {
        assert_eq!(
            select_rollback_ids(&[1, 2, 3], MigrationRollbackSelection::Steps(2)).unwrap(),
            vec![3, 2]
        );
        assert_eq!(
            select_rollback_ids(&[1, 2, 3], MigrationRollbackSelection::Target(2)).unwrap(),
            vec![3, 2]
        );
        assert!(select_rollback_ids(&[1, 3], MigrationRollbackSelection::Target(2)).is_err());
        assert!(select_rollback_ids(&[1], MigrationRollbackSelection::Steps(0)).is_err());
    }

    #[test]
    fn source_local_id_slug_and_sql_policy_use_the_real_generation_helpers() {
        assert_eq!(next_migration_id(None).unwrap(), 1);
        assert_eq!(next_migration_id(Some(41)).unwrap(), 42);
        assert!(next_migration_id(Some(u64::MAX)).is_err());
        assert_eq!(sanitize_name(" API Seed!! ").unwrap(), "api_seed");
        assert!(sanitize_name("___").is_err());
        assert!(normalize_sql("DROP INDEX \"demo\";").contains("DROP INDEX IF EXISTS"));
        assert!(validate_sql("CREATE TABLE demo(id BIGINT);").is_ok());
        assert!(validate_sql("ALTER TABLE c ADD REFERENCES p(id);").is_err());
    }

    #[test]
    fn registered_model_sets_are_discovered_as_migration_sources() {
        let code = "migration-auto-source-test";
        TcMgr::set_models(code, crate::models!(BaseDs));
        let root = PathBuf::from("/tmp/toasty-mgr-auto-sources");
        let codes = TcMigrationMgr::register_model_sources(MigrationSourcesConfig {
            artifact_root: root.clone(),
            migration_group: MigrationGroupKey("auto-source-test".to_owned()),
            backend_override: Some("postgresql".to_owned()),
            namespace: None,
            scope: SchemaScope::Managed,
        })
        .unwrap();

        assert_eq!(codes.first().map(String::as_str), Some(crate::BASE));
        assert!(codes.iter().any(|value| value == code));
        assert_eq!(
            TcMigrationMgr::source_codes().first().map(String::as_str),
            Some(crate::BASE)
        );
        assert_eq!(source_config(code).unwrap().artifact_root, root.join(code));
        TcMgr::unregister(code);
    }
}
