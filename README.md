# toasty-mgr

`toasty-mgr` is a process-wide Toasty connection manager. An application keeps
connection configuration in the built-in `base_ds` table of a reserved `base`
database, registers the Toasty models compiled into the application, and asks
for a `Db` by data-source code.

The crate depends on `toasty = "0.9.0"`. It does not depend on KX crates or
SeaORM and has no direct `toasty-driver-postgresql` dependency, so it can be
used by an application that already has its own SeaORM dependency. Enabling
`postgresql` lets Toasty select its own driver transitively.

## Add the dependency

Enable every backend used by the control database or a managed data source:

```toml
[dependencies]
anyhow = "1"
toasty-mgr = { path = "../toasty-mgr", features = ["postgresql", "mysql"] }
```

Available features are `sqlite`, `turso`, `mysql`, and `postgresql`. No driver
is enabled by default.

## Start the application

```rust,no_run
use toasty_mgr::{Model, TcMgr};

#[derive(Debug, Model)]
struct Customer {
    #[key]
    id: i64,
    name: String,
}

# async fn start(control_url: &str) -> anyhow::Result<()> {
// Register application models before a source can be loaded.
TcMgr::set_models("tenant_a", toasty_mgr::models!(Customer));

// `base` automatically uses models!(BaseDs).
TcMgr::register_base(control_url).await?;
TcMgr::health(toasty_mgr::BASE).await?;

// Cache miss -> base_ds lookup -> connect -> publish to the cache.
let mut tenant = TcMgr::get("tenant_a").await?;
let customers = Customer::all().exec(&mut tenant).await?;
# let _: Vec<Customer> = customers;
# Ok(())
# }
```

Provision `base_ds` separately with a migration and seed command. For a fresh
development database only, call `TcMgr::push_base_schema()` once after
`register_base`, then insert `BaseDs` rows. The
[application integration guide](docs/guide/src/application-integration.md)
contains the complete provisioning example.

## Schema 迁移使用方法

启用 `migration` 和实际使用的数据库 feature。内置迁移 backend 包括 `postgresql`、`mysql`、
`sqlite`、`turso`，每个 backend 独立负责 catalog introspection、类型归一化、账本、apply 和
rollback：

```toml
[dependencies]
toasty-mgr = {
    path = "../toasty-mgr",
    features = ["migration", "postgresql", "mysql", "sqlite", "turso"]
}
```

应用必须先注册全部 Toasty `ModelSet`，再注册迁移 source，最后建立数据库连接。普通源码工作区使用
`RegisteredFilesystem`，迁移根目录按 `<artifact_root>/<source_code>` 隔离：

```rust,no_run
use std::path::Path;

use toasty_mgr::migration::{
    MigrationGroupKey, MigrationSourcesConfig, SchemaScope, TcMigrationMgr,
};
use toasty_mgr::{BaseDs, TcMgr};

# async fn register(control_url: &str) -> anyhow::Result<()> {
TcMgr::set_models("auth", toasty_mgr::models!(BaseDs));
TcMgr::register_base(control_url).await?;

TcMigrationMgr::register_model_sources(MigrationSourcesConfig {
    artifact_root: Path::new("toasty").to_path_buf(),
    migration_group: MigrationGroupKey("primary-database".to_owned()),
    backend_override: None,
    namespace: None,
    scope: SchemaScope::Managed,
})?;
# Ok(())
# }
```

标准发布流程是先生成、审查和检查 artifact，再执行 tracked migration。调用方直接使用
`TcMigrationMgr` 的 outcome/report，不需要再声明镜像 DTO：

```rust,no_run
use toasty_mgr::migration::{
    MigrationApplyMode, MigrationGenerateRequest, SchemaOrigin, TcMigrationMgr,
};

# async fn migrate() -> anyhow::Result<()> {
let generated = TcMigrationMgr::generate(MigrationGenerateRequest {
    source: "auth".to_owned(),
    name: "add_login_index".to_owned(),
    origin: SchemaOrigin::Auto,
    ..MigrationGenerateRequest::default()
})
.await?;

TcMigrationMgr::check(Some("auth")).await?;
let applied = TcMigrationMgr::apply("auth", MigrationApplyMode::Execute).await?;
println!("created={}, applied={}", generated.created, applied.applied);
# Ok(())
# }
```

新项目或开发数据库可使用 `sync`，直接把 live database 收敛到当前模型。`sync` 不读取 latest
snapshot，不写 SQL/snapshot/history artifact，也不写迁移账本；它在内存中生成
`LiveDatabase -> CurrentModel` SQL，并通过对应 backend lock 执行。执行后 manager 会再次 introspect，
确认实时结构已经与模型一致。部分索引等 Toasty schema 无法表达的 backend 对象保留在数据库中，
不会被错误归一化为普通索引。

```rust,no_run
use toasty_mgr::migration::TcMigrationMgr;

# async fn sync_schema() -> anyhow::Result<()> {
let dry_run = TcMigrationMgr::sync("auth", true).await?;
println!("sql={:?}", dry_run.sql);

let applied = TcMigrationMgr::sync("auth", false).await?;
println!("changed={}", applied.changed);

// 按 base 优先、其余 source code 稳定排序处理全部已注册 source。
let all = TcMigrationMgr::sync_all(false).await?;
println!("sources={}", all.len());
# Ok(())
# }
```

单物理数据源模式不需要数字 ID 分段。每个 logical source 使用自己的 artifact 目录和 source-local ID，
数据库账本使用 `__toasty_mgr_migrations(source_code, id)` 复合主键；`SchemaScope::Managed` 只观察
当前 source 的模型表，因此不同 source 可安全共享同一个 database/schema。

状态与回滚示例：

```rust,no_run
use toasty_mgr::migration::{MigrationRollbackSelection, TcMigrationMgr};

# async fn inspect_and_rollback() -> anyhow::Result<()> {
let status = TcMigrationMgr::status("auth", true).await?;
println!("pending={:?}, drift={}", status.pending, status.model_drift);

let report = TcMigrationMgr::rollback("auth", MigrationRollbackSelection::Steps(1)).await?;
println!("rolled_back={}", report.rolled_back);
# Ok(())
# }
```

关键限制：

- PostgreSQL migration SQL 可直接使用普通分号分隔多条 statement，也兼容
  `-- #[toasty::breakpoint]` 显式边界；
- 禁止在 managed schema 中使用数据库外键、`REFERENCES` 或 cascade；
- PostgreSQL、SQLite、Turso 的 DDL 与账本记录使用事务边界；
- MySQL DDL 会 implicit commit，adapter 使用 database 级 `GET_LOCK` 和
  `__toasty_mgr_migration_runs`；发现异常遗留 marker 时返回 `migration_recovery_ambiguous`，不会猜测重试；
- `sync` 是不写正式 history 的即时数据库同步能力；正式发布仍必须使用
  `generate`、审查 artifact、`check` 和 `apply`。

## Project documentation

- [Application integration](docs/guide/src/application-integration.md): startup
  order, catalog provisioning, service-layer use, and configuration rules.
- [Complete application example](docs/guide/src/complete-example.md): a
  copyable multi-data-source example including transactions and operations.
- [Operations runbook](docs/guide/src/operations.md): reload, aliases, health,
  removal, logging, and failure diagnosis.
- [Control table](docs/guide/src/catalog.md): `BaseDs` fields, backend mapping,
  password handling, and production constraints.
- [Testing](docs/guide/src/testing.md): SQLite/Turso tests and opt-in local
  MySQL/PostgreSQL tests.

The complete compile-checked example is
[`docs/templates/application.rs`](docs/templates/application.rs). Toasty model
and CRUD syntax is documented by the
[upstream Toasty project](https://github.com/tokio-rs/toasty); this repository
documents the connection-management layer.

## Important constraints

- A `ModelSet` must be registered before the first `get` for each source.
- `push_base_schema` is for a fresh database, not an idempotent startup step.
- `BaseDs.pwd` is plaintext unless the application installs a
  `PasswordResolver`.
- PostgreSQL URLs generated from `BaseDs` currently contain
  `sslmode=disable`. Use explicit URL registration for TLS-required sources.
- `TcTxMgr` coordinates transactions but does not provide distributed atomicity.
- Reusable statement helpers should accept `&mut dyn Executor`; retry complete
  optimistic transactions with `trans_on_condition_failed`.
