---
name: design
description: Create or review a user-facing toasty-mgr design document for API, BaseDs schema, lifecycle, backend, security, transaction, or compatibility changes before implementation.
---

# Design toasty-mgr changes

Copy `docs/templates/design.md` to `docs/design/<feature-name>.md`.

Write for applications that register and operate managed Toasty connections.
Describe observable behavior rather than modules or implementation steps.

## Required analysis

- Show the public call sequence and Cargo features.
- State changes to `BaseDs` and existing table compatibility.
- Cover cache publication, lazy loading, reload, aliases, health, and removal.
- Compare SQLite, Turso, MySQL, and PostgreSQL behavior.
- Define password, URL redaction, TLS, and logging effects.
- State whether cross-data-source commits can partially succeed.
- Preserve the no-KX and no-SeaORM dependency boundary.
- Include unit, in-memory, and local-database test coverage.

Use illustrative Rust blocks without hidden doctest boilerplate. After the
feature ships, move durable user guidance into `docs/guide/src/` and remove
temporary design claims that no longer add value.
