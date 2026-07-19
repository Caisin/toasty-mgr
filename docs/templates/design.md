<!--
Copy this file to docs/design/<feature-name>.md.
Describe user-visible behavior. Do not turn this into an implementation plan.
Delete sections only when they do not apply.
-->

# {Feature name}

## Summary

Describe the user-visible change and its purpose in one paragraph.

## Motivation

Describe the concrete problem. Include examples of configurations or calls that
are impossible, unsafe, or surprising without this change.

## User-facing API

Write this section in guide style. Show the public calls, required feature flags,
and the order in which users invoke them. Include a before/after example when an
existing API changes.

```rust,ignore
// Show only the relevant application code.
```

## Catalog changes

List changes to `BaseDs`, including field types, nullability, defaults, URL
construction, and compatibility with existing `base_ds` tables.

## Lifecycle behavior

Describe effects on registration, lazy loading, aliases, reload, health checks,
removal, and model-set lookup.

## Backend behavior

State the behavior for SQLite, Turso, MySQL, and PostgreSQL. Separate capability
differences from implementation details.

## Failure behavior

Describe validation errors, connection failures, cache publication rules, and
whether an old connection remains usable after failure.

## Security

Cover password handling, URL redaction, TLS effects, and logging. State which
values can leave process memory or appear in diagnostics.

## Compatibility

State whether the change affects existing tables, public APIs, Cargo features,
or the guarantee that this crate has no KX/SeaORM dependency.

## Test plan

List unit tests, SQLite/Turso end-to-end tests, and ignored MySQL/PostgreSQL local
tests. Include cleanup requirements for external databases.

## Documentation

List guide chapters, rustdoc, templates, and skills that must change when the
feature lands.
