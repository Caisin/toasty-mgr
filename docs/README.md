# toasty-mgr documentation

These docs explain how an application integrates and operates `toasty-mgr`.
They intentionally do not duplicate Toasty's model and CRUD guide.

Start here:

- [`guide/src/application-integration.md`](./guide/src/application-integration.md)
  for dependency setup, startup order, catalog provisioning, and service use.
- [`guide/src/complete-example.md`](./guide/src/complete-example.md) for a
  complete downstream application example.
- [`guide/src/operations.md`](./guide/src/operations.md) for reload, health,
  aliases, removal, and troubleshooting.
- [`guide/src/architecture.md`](./guide/src/architecture.md) for cache-miss and
  connection-publication behavior.

The guide is an mdBook rooted at [`guide/src/SUMMARY.md`](./guide/src/SUMMARY.md).
Build it with:

```bash
mdbook build docs/guide
```

Compile-checked Rust templates live in [`templates/`](./templates/):

- [`application.rs`](./templates/application.rs): end-to-end downstream usage.
- [`bootstrap.rs`](./templates/bootstrap.rs): minimal startup and provisioning.
- [`local-database-test.rs`](./templates/local-database-test.rs): ignored local
  MySQL/PostgreSQL integration test.
- [`design.md`](./templates/design.md): user-facing design proposal structure.

Repository-local authoring and testing skills live under
[`../.agents/skills/`](../.agents/skills/).
