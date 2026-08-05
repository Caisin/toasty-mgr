#![cfg(all(
    feature = "migration",
    any(feature = "mysql", feature = "sqlite", feature = "turso")
))]

use std::{collections::BTreeSet, path::PathBuf};

use anyhow::Result;
use toasty_mgr::{
    BaseDs, TcMgr,
    migration::{
        MigrationApplyMode, MigrationArtifactInput, MigrationArtifactSet, MigrationBackend,
        MigrationGroupKey, MigrationRollbackSelection, MigrationSourceConfig, ObservedSchema,
        OwnedMigrationFile, SchemaInspectRequest, SchemaScope, TcMigrationMgr,
    },
};

#[cfg(feature = "mysql")]
use toasty_mgr::migration::MySqlMigrationBackend;
#[cfg(feature = "sqlite")]
use toasty_mgr::migration::SqliteMigrationBackend;
#[cfg(feature = "turso")]
use toasty_mgr::migration::TursoMigrationBackend;

#[cfg(feature = "mysql")]
#[tokio::test]
#[ignore = "requires TOASTY_TEST_MYSQL_URL"]
async fn mysql_adapter_applies_inspects_and_rolls_back() -> Result<()> {
    let url = std::env::var("TOASTY_TEST_MYSQL_URL")?;
    let source = "migration-mysql-adapter-test";
    let table = source.replace('-', "_");
    let index = format!("{table}_name");
    let models = toasty_mgr::models!(BaseDs);
    let mut db = TcMgr::add_by_url_with_models(source, &url, models).await?;
    TcMigrationMgr::register_source(MigrationSourceConfig {
        code: source.to_owned(),
        artifact_root: PathBuf::from("/tmp/toasty-mgr-adapter-tests").join(source),
        migration_group: MigrationGroupKey("adapter-test-mysql".to_owned()),
        backend_override: Some("mysql".to_owned()),
        namespace: None,
        scope: SchemaScope::Tables(BTreeSet::from([table.clone()])),
    })?;
    TcMigrationMgr::set_registered_artifacts(
        source,
        MigrationArtifactInput::Owned(MigrationArtifactSet::new(vec![OwnedMigrationFile {
            id: 1,
            name: format!("{source}_1_probe.sql"),
            sql: format!(
                "CREATE TABLE `{table}` (\
                    `id` BIGINT UNSIGNED NOT NULL AUTO_INCREMENT, \
                    `name` VARCHAR(191) NOT NULL, PRIMARY KEY (`id`));\n\
                 -- #[toasty::breakpoint]\n\
                 CREATE UNIQUE INDEX `{index}` ON `{table}` (`name`);"
            ),
            rollback_sql: Some(format!("DROP TABLE `{table}`;")),
            snapshot_name: None,
        }])?),
    )?;

    assert_eq!(
        TcMigrationMgr::apply(source, MigrationApplyMode::Execute)
            .await?
            .applied,
        1
    );
    assert_eq!(
        TcMigrationMgr::apply(source, MigrationApplyMode::Execute)
            .await?
            .skipped,
        1
    );
    let backend = MySqlMigrationBackend::default();
    let managed_tables = vec![table.clone()];
    let scope = SchemaScope::Tables(BTreeSet::from([table.clone()]));
    let observed = backend
        .inspect(SchemaInspectRequest {
            source_code: source,
            namespace: None,
            scope: &scope,
            managed_tables: &managed_tables,
            db: &mut db,
        })
        .await?;
    assert_observed_probe(&observed, &table, &index);
    backend.normalize(&observed, &db.schema().db)?;

    assert_eq!(
        TcMigrationMgr::rollback(source, MigrationRollbackSelection::Steps(1))
            .await?
            .rolled_back,
        1
    );
    assert_eq!(TcMigrationMgr::status(source, false).await?.pending, [1]);
    TcMgr::unregister(source);
    Ok(())
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_adapter_applies_inspects_and_rolls_back() -> Result<()> {
    let path = std::env::temp_dir().join(format!(
        "toasty-mgr-migration-sqlite-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let result = exercise_sqlite_family(
        "migration-sqlite-adapter-test",
        &format!("sqlite:{}", path.display()),
        "sqlite",
        &SqliteMigrationBackend::default(),
    )
    .await;
    let _ = std::fs::remove_file(path);
    result
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_sync_generates_from_live_schema_and_applies_idempotently() -> Result<()> {
    let suffix = std::process::id();
    let source = format!("migration-sqlite-sync-{suffix}");
    let database_path = std::env::temp_dir().join(format!("toasty-mgr-sync-{suffix}.db"));
    let artifact_root = std::env::temp_dir().join(format!("toasty-mgr-sync-{suffix}"));
    let _ = std::fs::remove_file(&database_path);
    let _ = std::fs::remove_dir_all(&artifact_root);

    let models = toasty_mgr::models!(BaseDs);
    TcMgr::add_by_url_with_models(
        &source,
        &format!("sqlite:{}", database_path.display()),
        models,
    )
    .await?;
    TcMigrationMgr::register_source(MigrationSourceConfig {
        code: source.clone(),
        artifact_root: artifact_root.clone(),
        migration_group: MigrationGroupKey(format!("sync-test-{suffix}")),
        backend_override: Some("sqlite".to_owned()),
        namespace: None,
        scope: SchemaScope::Managed,
    })?;

    let (generated, applied) = TcMigrationMgr::sync(&source, "live_sync").await?;
    assert!(generated.created);
    assert_eq!(applied.applied, 1);
    assert_eq!(applied.adopted, 0);
    let status = TcMigrationMgr::status(&source, true).await?;
    assert_eq!(status.applied, 1);
    assert!(status.pending.is_empty());
    assert!(!status.model_drift);

    let (generated, applied) = TcMigrationMgr::sync(&source, "live_sync").await?;
    assert!(!generated.created);
    assert_eq!(applied.applied, 0);
    assert_eq!(applied.skipped, 1);

    TcMgr::unregister(&source);
    let _ = std::fs::remove_file(database_path);
    let _ = std::fs::remove_dir_all(artifact_root);
    Ok(())
}

#[cfg(feature = "turso")]
#[tokio::test]
async fn turso_adapter_applies_inspects_and_rolls_back() -> Result<()> {
    exercise_sqlite_family(
        "migration-turso-adapter-test",
        "turso::memory:",
        "turso",
        &TursoMigrationBackend::default(),
    )
    .await
}

async fn exercise_sqlite_family(
    source: &str,
    url: &str,
    backend_id: &str,
    backend: &dyn MigrationBackend,
) -> Result<()> {
    let table = source.replace('-', "_");
    let index = format!("{table}_name");
    let models = toasty_mgr::models!(BaseDs);
    let mut db = TcMgr::add_by_url_with_models(source, url, models).await?;
    TcMigrationMgr::register_source(MigrationSourceConfig {
        code: source.to_owned(),
        artifact_root: PathBuf::from("/tmp/toasty-mgr-adapter-tests").join(source),
        migration_group: MigrationGroupKey(format!("adapter-test-{source}")),
        backend_override: Some(backend_id.to_owned()),
        namespace: None,
        scope: SchemaScope::Tables(BTreeSet::from([table.clone()])),
    })?;
    TcMigrationMgr::set_registered_artifacts(
        source,
        MigrationArtifactInput::Owned(MigrationArtifactSet::new(vec![OwnedMigrationFile {
            id: 1,
            name: format!("{source}_1_probe.sql"),
            sql: format!(
                "CREATE TABLE \"{table}\" (\
                    \"id\" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT, \
                    \"name\" TEXT NOT NULL);\n\
                 -- #[toasty::breakpoint]\n\
                 CREATE UNIQUE INDEX \"{index}\" ON \"{table}\" (\"name\");"
            ),
            rollback_sql: Some(format!("DROP TABLE \"{table}\";")),
            snapshot_name: None,
        }])?),
    )?;

    let applied = TcMigrationMgr::apply(source, MigrationApplyMode::Execute).await?;
    assert_eq!(applied.applied, 1);
    let repeated = TcMigrationMgr::apply(source, MigrationApplyMode::Execute).await?;
    assert_eq!(repeated.skipped, 1);

    let managed_tables = vec![table.clone()];
    let scope = SchemaScope::Tables(BTreeSet::from([table.clone()]));
    let observed = backend
        .inspect(SchemaInspectRequest {
            source_code: source,
            namespace: None,
            scope: &scope,
            managed_tables: &managed_tables,
            db: &mut db,
        })
        .await?;
    assert_observed_probe(&observed, &table, &index);
    let normalized = backend.normalize(&observed, &db.schema().db)?;
    assert_eq!(normalized.tables.len(), 1);
    assert!(normalized.tables[0].columns[0].auto_increment);

    let status = TcMigrationMgr::status(source, false).await?;
    assert_eq!(status.applied, 1);
    assert!(status.pending.is_empty());

    let rollback = TcMigrationMgr::rollback(source, MigrationRollbackSelection::Steps(1)).await?;
    assert_eq!(rollback.rolled_back, 1);
    let status = TcMigrationMgr::status(source, false).await?;
    assert_eq!(status.applied, 0);
    assert_eq!(status.pending, [1]);

    TcMgr::unregister(source);
    Ok(())
}

fn assert_observed_probe(observed: &ObservedSchema, table: &str, index: &str) {
    assert!(
        observed.diagnostics.is_empty(),
        "{:?}",
        observed.diagnostics
    );
    let observed_table = observed
        .tables
        .iter()
        .find(|candidate| candidate.name == table)
        .expect("adapter should inspect the managed probe table");
    assert_eq!(observed_table.columns.len(), 2);
    assert!(observed_table.columns[0].auto_increment);
    assert!(
        observed_table
            .indices
            .iter()
            .any(|candidate| candidate.primary_key)
    );
    assert!(
        observed_table
            .indices
            .iter()
            .any(|candidate| candidate.name == index && candidate.unique)
    );
}
