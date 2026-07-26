---
name: write-model
description: Design or revise Toasty models for toasty-mgr applications from storage, query, relationship, uniqueness, lifecycle, and database-backend requirements. Use when creating `#[derive(Model)]` or `#[derive(Embed)]` types, choosing Toasty field attributes, registering a `ModelSet`, or adopting Toasty 0.9 model and query features.
---

# Write Toasty models

Start from the application's invariants and access patterns. Do not copy every
attribute from the template into every model.

## Workflow

1. Inspect `Cargo.toml` and `Cargo.lock` for the exact Toasty version and enabled
   backend, `serde`, `jiff`, decimal, and migration features.
2. Read the surrounding domain types, queries, migrations, and registered
   `ModelSet`. Preserve existing table and column names unless a migration is
   part of the request.
3. Write down the primary key, uniqueness rules, common filters and sort orders,
   relation cardinality, optionality, mutation paths, and target databases.
4. Choose storage deliberately:
   - Use a normal field for one scalar column.
   - Use `Embed` for a value object whose members need separate columns.
   - Use `#[document]` for an embedded struct stored as one document but queried
     through typed nested paths.
   - Use `Json<T>` for typed opaque JSON and `serde_json::Value` for dynamic JSON.
5. Add only indexes justified by reads, uniqueness, relation traversal, or
   upsert targets. Index every foreign-key path; use a composite index when a
   composite relation needs all columns together.
6. Make relation ownership explicit. Put the foreign key and `belongs_to` on the
   child, and the matching `has_many` or `has_one` on the parent. Use
   `Deferred<_>` on cycles and for relations that should load on demand.
7. Register all roots in the data source's `toasty_mgr::models!(...)`, then
   update the schema through the project's migration/provisioning path.

Use `docs/templates/toasty-model.rs` as a compile-checked example. Read
[`references/toasty-0.9.md`](references/toasty-0.9.md) before using a 0.9-only
feature or when choosing backend-specific storage.

## Required checks

- A root model has exactly one primary-key definition.
- Every required create field has an input, `#[default(...)]`, `#[update(...)]`,
  or `#[auto]` strategy.
- JSON fields declare `#[column(type = text|json|jsonb)]`; Toasty 0.9 rejects an
  implicit JSON column type.
- Integer enum discriminants are unique, fit the selected integer type, and are
  assigned on every variant.
- `#[shared(name)]` members have the same Rust type; shared-field indexes live on
  the enum, not on individual variants.
- `#[document]` contains only supported embedded structs/scalars and is not
  combined with `#[column]`, `#[index]`, `#[unique]`, relations, keys, versions,
  or auto generation.
- Optional relations use `Option` consistently in both the foreign key and the
  relation target. A relation intended for `remove()` normally needs a nullable
  foreign key.
- Backend limitations are stated next to the design: targeted upsert is not
  supported by MySQL, and native JSON/JSONB types are backend-specific.

## Verification

Add or update a compile-checked template or focused test when introducing a new
model pattern. Run:

```bash
cargo fmt --all -- --check
cargo test --all-features --all-targets
cargo clippy --all-features --all-targets -- -D warnings
```

If a model changes an existing table, also verify the generated migration or
schema diff against representative existing data. Do not treat `push_schema()`
as a production migration plan.
