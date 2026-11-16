---
name: broccoli-docs-maintainer
description:
  Use when writing maintainer-facing Broccoli docs about the host platform
  internals. Lives in the Internals section. Read docs/authoring/house-style.md
  first.
---

# Maintainer-facing docs

For people working on the host platform itself. The reader has deep context or
is building it.

Read `docs/authoring/house-style.md` and `docs/authoring/diagrams.md` first.
This skill adds only the maintainer specifics.

## Voice

Terse, exact, high trust. Document the non obvious: architecture, data flow,
invariants, the reasons behind decisions, the gotchas. Never explain basic
programming. Link to real files and modules.

## Punctuation

Em dashes banned. Colons and hyphens are fine.

## Ground in these sources

- `packages/server/`, `packages/worker/`, `packages/mq/`, `packages/common/` for
  the host platform.
- `packages/plugin-core/` for how plugins are loaded and run.
- `packages/api-types/` for the REST request and response types.
- The `justfile` and `docs/` for build and release reality.

## Location and shape

- English `website/docs/<section>/<name>.md`. Chinese mirror under the i18n
  path.
- Section in the sidebar: Internals.
- Favor explanation and rationale over tutorials. Capture invariants and the
  why.

## Wire it in

Add the page id to `website/sidebars.ts` under the Internals section.
