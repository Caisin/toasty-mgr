# Toasty 0.9 modeling reference

This repository resolves Toasty `0.9.0`. The release is documented in the
upstream [0.9.0 changelog](https://github.com/tokio-rs/toasty/blob/toasty-v0.9.0/crates/toasty/CHANGELOG.md)
and [guide](https://tokio-rs.github.io/toasty/0.9.0/guide/).

## Model and storage features

| Feature | Use it when | Example | Important constraint |
|---|---|---|---|
| Native/dynamic JSON | The value is intentionally opaque or schemaless | `#[column(type = jsonb)] payload: serde_json::Value` | `text`, `json`, or `jsonb` is mandatory in 0.9 |
| Typed JSON | Rust owns the serialized shape but nested fields are not queried | `#[column(type = json)] settings: Json<Settings>` | Enable `toasty/serde`; `T` must implement serde traits |
| Document embed | A structured value is one document column and nested fields are queried | `#[document] profile: Profile` | Embed structs only; no enum embeds or field index yet |
| Integer enum | Stable numeric discriminants are part of the storage contract | `#[column(variant = 10)] Low` | Every variant needs a unique integer value |
| Shared enum field | Data-carrying variants represent the same logical field | `#[shared(address)] email: String` | Shared members need the same type and column mapping |
| Enum-level index | A shared or variant field is a lookup/uniqueness path | `#[unique(address)]` | Shared-field indexes belong on the enum |
| Temporal vector | A model stores several `jiff` temporal values | `seen_at: Vec<jiff::Timestamp>` | Enable both the application `jiff` dependency and `toasty/jiff` |

Use `jsonb` for PostgreSQL, `json` for PostgreSQL/MySQL native JSON, and `text`
when a text-backed representation is intentional. Do not claim a storage type is
portable without checking every enabled driver.

`#[document]` differs from `Json<T>`: a document embed keeps Toasty's typed path
API, so a query can use `Model::fields().profile().region().eq("ap-east")`.
`Json<T>` is an opaque scalar at the Toasty query layer.

## Query and mutation features

### Targeted upsert

`#[key]` and `#[unique]` generate `upsert_by_*` constructors:

```rust
Account::upsert_by_email("ops@example.com")
    .display_name("Operations")
    .exec(&mut db)
    .await?;
```

PostgreSQL, SQLite, and Turso support key/unique targets and branch-specific
assignments. DynamoDB supports a narrower primary-key form. MySQL does not
support Toasty's targeted upsert API.

### Filtered and ordered include

Modify the relation field passed to `include`:

```rust
Workspace::all()
    .include(
        Workspace::fields()
            .tasks()
            .filter(Task::fields().completed().eq(false))
            .order_by(Task::fields().id().asc()),
    )
    .exec(&mut db)
    .await?;
```

This restricts and orders the related rows loaded for each parent. It does not
filter the parent rows.

### Deferred relation mutation

`insert` and `remove` return statements in 0.9; they do nothing until `exec`:

```rust
workspace.tasks().insert(&task).exec(&mut db).await?;
workspace.tasks().remove(&task).exec(&mut db).await?;
```

Only direct relation traversals are mutable. A `via` relation is read-only.
Removing a direct relation must also be valid for the foreign-key column, which
usually means that the child foreign key is nullable.

## Selection rules

- Prefer a normalized relation when members have independent identity,
  lifecycle, indexing, or high-cardinality updates.
- Prefer a flattened `Embed` when every member is part of the parent's stable
  relational schema and needs ordinary indexes.
- Prefer `#[document]` when the value is owned by the parent, updated together,
  and needs typed nested filtering without a separate table.
- Prefer `Json<T>` or `serde_json::Value` when Toasty should not understand the
  nested shape.
- Prefer string enum labels when operators inspect the database manually;
  prefer integer discriminants only when stable numeric codes are an explicit
  compatibility contract.
