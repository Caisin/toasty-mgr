# toasty-mgr

`toasty-mgr` is a process-wide Toasty connection manager. An application keeps
connection configuration in the built-in `base_ds` table of a reserved `base`
database, registers the Toasty models compiled into the application, and asks
for a `Db` by data-source code.

The crate depends on `toasty = "0.8.0"`. It does not depend on KX crates or
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
