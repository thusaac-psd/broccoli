---
name: broccoli-docs-plugin-dev
description:
  Use when writing plugin-developer Broccoli docs for people building plugins on
  the SDKs. Lives in the Building plugins section. Read
  docs/authoring/house-style.md first.
---

# Plugin-developer docs

For people who write code well but know nothing of Broccoli internals.

Read `docs/authoring/house-style.md` and `docs/authoring/diagrams.md` first.
This skill adds only the plugin-developer specifics.

## Voice

Precise, practical, peer to peer. Code first, with real runnable examples drawn
from the actual SDK surface, the real manifest, the real handler signatures, the
real build commands. Explain the why behind each contract, for example why
plugin HTTP responses are JSON only, so the reader stops fighting the platform.

## Punctuation

Em dashes banned. Colons and hyphens are fine.

## Ground in these sources

- `packages/server-sdk/` and its `ARCHITECTURE.md` for the backend plugin API.
- `packages/web-sdk/` for the frontend slot system.
- `packages/plugin-core/` for the manifest and runtime.
- `packages/dev-cli/` (the `broccoli-dev` binary) for scaffold, build, and watch
  commands.
- `packages/broccoli-types/` for the types the guest SDK and the host share.
- Existing plugins under `plugins/` as worked examples.

## Location and shape

- English `website/docs/<section>/<name>.md`. Chinese mirror under the i18n
  path.
- Section in the sidebar: Building plugins.
- Mix task walkthroughs with accurate reference. A getting-started page shows a
  plugin built end to end.

## Wire it in

Add the page id to `website/sidebars.ts` under the Building plugins section.
