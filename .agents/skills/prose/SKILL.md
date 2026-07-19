---
name: prose
description: Author or edit toasty-mgr documentation, READMEs, design docs, release notes, PR descriptions, or other human-readable prose using the repository writing conventions.
---

# Write toasty-mgr prose

Use direct, factual, present-tense language.

## Style

- State what an API does and when to call it.
- Prefer worked examples over claims about ease or flexibility.
- Use active voice and concrete nouns.
- Remove buzzwords, filler, history, and planned behavior.
- Name limitations directly, especially transaction atomicity, schema creation,
  password handling, TLS, and backend differences.
- Keep code terms, paths, features, environment variables, and data-source codes
  in backticks.

## Structure

1. Define the concept.
2. Explain the problem it solves.
3. Show the minimum working call sequence.
4. Explain lifecycle and failure behavior.
5. State backend or security constraints.

Use the chapter organization in `docs/guide/src/SUMMARY.md`. Put reusable
authoring material in `docs/templates/`, not inside guide chapters.

## Scope

- Write from the downstream application's point of view: what it configures,
  when it calls the manager, and how it handles failure.
- Keep startup, catalog provisioning, service use, operations, and testing
  examples specific to `toasty-mgr`.
- Do not reproduce Toasty's model, relationship, query, or CRUD guide. Link to
  the upstream Toasty documentation after showing where the managed `Db` enters
  application code.
- Add complete reusable examples to `docs/templates/` and compile them through
  `tests/doc_templates.rs`.
