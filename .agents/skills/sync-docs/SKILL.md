---
name: sync-docs
description: Synchronize toasty-mgr guide chapters, rustdoc, README examples, skills, and templates with public API, catalog schema, Cargo feature, backend, lifecycle, or test changes.
---

# Synchronize toasty-mgr documentation

Apply the repository `prose` skill before editing documentation.

## Scan

1. Inspect `git diff` and recent commits for changes to `src/`, `Cargo.toml`, and
   `tests/`.
2. Classify user-visible changes: public API, `BaseDs` fields, model-set rules,
   alias behavior, reload/health behavior, transactions, password handling,
   Cargo features, backend URLs, or cleanup behavior.
3. Search `README.md`, `docs/`, rustdoc, `.agents/skills/`, and
   `docs/templates/` for claims affected by each change.

## Update surfaces

- Update application integration, the complete example, operations,
  architecture, or API reference under `docs/guide/src/` according to the
  user-visible behavior.
- Update `docs/guide/src/SUMMARY.md` when chapters change.
- Update public rustdoc when the API contract changes.
- Update `README.md` only as the short entry point; keep detail in the guide.
- Update skills and compile-checked templates when their commands, paths, or
  examples drift.
- Keep Toasty ORM tutorials out of this guide; link upstream for model and CRUD
  details after showing how the application gets a managed `Db`.
- Describe current behavior without "recently", "now", or version-history prose.

## Verify

Run:

```bash
cargo fmt --all -- --check
cargo test --all-features --all-targets
cargo clippy --all-features --all-targets -- -D warnings
cargo doc --all-features --no-deps
mdbook build docs/guide
```

If `mdbook` is unavailable, check Markdown links and code fences with available
repository tools and report the missing build step.
