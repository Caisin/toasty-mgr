---
name: write-tests
description: Write or edit toasty-mgr unit, in-memory backend, or local MySQL/PostgreSQL integration tests while preserving database cleanup and credential-safety rules.
---

# Write toasty-mgr tests

Prefer public-API integration tests for lifecycle behavior. Use inline unit tests
for URL construction, registry validation, redaction, and transaction invariants.

## Locations

| Behavior | Location |
|---|---|
| URL, metadata, alias validation, pool options | inline `#[cfg(test)]` |
| SQLite control-table lifecycle | `tests/sqlite_manager.rs` |
| Turso control-table lifecycle | `tests/turso_manager.rs` |
| Real MySQL/PostgreSQL lifecycle | `tests/local_databases.rs` |

## Required lifecycle coverage

For a managed data-source test:

1. Register `base`.
2. Provision or verify `base_ds`.
3. Register the managed source `ModelSet`.
4. Insert a unique enabled `BaseDs` row.
5. Call `TcMgr::get(code)` and `TcMgr::health(code)`.
6. Remove the connection and temporary row even when the assertion fails.
7. Drop `base_ds` only if the test created it.

Never hardcode credentials. Read local URLs from `TOASTY_TEST_MYSQL_URL` and
`TOASTY_TEST_POSTGRES_URL`. Mark external-service tests `#[ignore]`; keep SQLite
and Turso tests enabled by their features.

Use `toasty_mgr::create!` for catalog setup. Test observable behavior through
`TcMgr` rather than private registry implementation details.

Run the focused test first, then:

```bash
cargo test --all-features --all-targets
cargo clippy --all-features --all-targets -- -D warnings
```

Use `docs/templates/local-database-test.rs` as a starting point and compare
cleanup behavior with `tests/local_databases.rs` before finishing.
