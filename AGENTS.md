# AGENTS.md

Canonical guidance for coding agents working in this repository. This is based
on the current repo layout and tooling, with `CLAUDE.md` used as supporting
project context rather than copied wholesale.

## Non-negotiables

- Never run `git stash`, `git reset --hard`, or other destructive commands
  unless the user explicitly asks for them.
- Preserve unrelated local changes.
- Prefer small, targeted edits over broad refactors.
- Validate with the smallest relevant command before finishing.

## Current Repo Shape

Broccoli is a mixed Rust + pnpm monorepo for an online judge with a WASM plugin
system.

### Rust workspace members

- `packages/common`
- `packages/mq`
- `packages/plugin-core`
- `packages/server`
- `packages/test-support` (dev-dependency harness shared by the server's
  `tests/integration` and `tests/e2e` trees)
- `packages/worker`
- `packages/cli`

Also present but not part of the main Rust workspace members list:

- `packages/server-sdk`
- plugin crates under `plugins/`
- additional plugin-related work under `packages/plugins/`

### JavaScript workspaces

- `packages/web-sdk`
- `packages/web`

## Commands That Actually Exist

Use `just` recipes when convenient; they map cleanly to the real commands.

### Rust

```bash
cargo build
cargo test --workspace
cargo test -p server
cargo test -p plugin-core
cargo clippy --workspace
cargo run -p server
cargo run -p worker
```

### JavaScript / frontend

```bash
pnpm install
pnpm build
pnpm dev
pnpm lint
pnpm format
pnpm format:check
pnpm test
pnpm typecheck
pnpm --filter @broccoli/web-sdk build
pnpm --filter @broccoli/web dev
```

### Justfile shortcuts

```bash
just build
just test
just clippy
just server
just worker
just build-js
just dev-web
just build-plugins
just build-plugins-release
just up
just down
```

### Infrastructure

```bash
docker compose up -d
```

Current `docker-compose.yaml` services:

- Postgres 17 on `127.0.0.1:5433`
- Redis 7 on `6379`
- SeaweedFS on `9333` and `8333`

The checked-in Docker Postgres credentials are:
`postgres://postgres:broccoli_pg_secret@localhost:5433/broccoli`

## Architecture Notes From The Current Tree

### Backend

- `packages/server` is the main axum server.
- `packages/worker` consumes MQ tasks and operation tasks in a reconnect loop.
- `packages/plugin-core` contains shared plugin abstractions: `config`, `error`,
  `hook`, `host`, `http`, `i18n`, `manager`, `manifest`, `registry`, and
  `traits`.
- `packages/cli` builds the `broccoli` binary and includes plugin build and
  related utility flows.
- `packages/server-sdk` exists as a separate Rust crate and should not be
  forgotten when changing public server-facing APIs.

### Frontend

- `packages/web` uses React 19, React Router 7 (SPA mode, `ssr: false`), Vite 8,
  Tailwind, TanStack Query, Monaco, Radix UI, and Recharts.
- Routing is file-system based through `@react-router/fs-routes`, with
  `packages/web/src/routes.ts` adding an explicit catch-all extension route.
- `packages/web-sdk` (`@broccoli/web-sdk`) is a published TypeScript package
  built with `tsdown`; the web app and plugin frontends build against it.
- Unit tests are `*.test.ts` files run by `pnpm test` (`node --test` with type
  stripping), so a tested module may use the `@/` alias only in `import type`;
  runtime imports must be packages or relative `.ts` paths.

### Plugins

- Root `plugins/` currently contains plugin crates such as `batch-evaluator`,
  `cooldown`, `ioi`, `standard-checkers`, and `submission-limit`, plus manifest
  only plugins such as `broccoli-zh-cn`.
- Plugin work also exists under `packages/plugins/`; do not assume everything
  plugin-related lives under the root `plugins/` folder.

## Working Rules

### Backend changes

- Keep handler flow consistent: docs, tracing, auth, validation, DB/business
  logic, response.
- Use the project’s structured error patterns and treat error `code` values as
  API contracts.
- Prefer permission helpers instead of checking roles directly.
- Think about transaction boundaries before editing write paths.
- When changing plugin execution behavior, check both manifest-level permissions
  and host-side registration/resolution.

### Frontend changes

- Keep changes aligned with the existing React Router 7 file-based structure.
- Do not describe the frontend as “plain Vite React” in docs or code comments;
  this package is using React Router’s framework tooling on top of Vite.
- Preserve the existing Tailwind/Radix/component patterns already in
  `packages/web`.

### SDK / API surface changes

- If you change server request/response shapes, check whether
  `packages/server-sdk` and `packages/web-sdk` need to move with them.
- Be careful with exported package entrypoints in `packages/web-sdk`, since its
  `package.json` exposes multiple subpaths, and plugin bundles resolve them
  through `SDK_SHARED_DEPS` in `packages/web/plugins/shared-deps.ts`.

### Duplication discipline

A July 2026 audit found the repo's real duplication risk is _aware copying_
(copies that cite their twin in a comment), not failed discovery — including one
production drift where two forked `Verdict::severity` tables ranked plugin
custom verdicts oppositely on host vs guest. Rules:

- Rule of three: the moment you write the third copy of a pattern, stop and
  extract it instead (or, if extraction is genuinely blocked by a boundary such
  as WASM guest/host or cargo's per-tree test binaries, say so in a comment at
  the copy site and add the pair to `docs/plans/dedup-backlog.md`).
- Never copy a block of ~20+ lines between files without either extracting it or
  leaving a comment that links the copies AND an entry in the dedup backlog. A
  silent copy is how the `Other(_) => 5` vs `Other(_) => 255` severity fork
  happened.
- Reviewers: for any new helper, grep for an existing implementation first
  (`severity`, `push_judge_sets`, and the judgement-column clear list were all
  trivially greppable when their second copies were written). Flag hand-rolled
  duplicates of shared utilities as blocking findings.
- Cross-boundary mirrors (guest SDK vs host) must live in `broccoli-types` when
  possible; if a verbatim mirror is unavoidable, both sides must carry a
  drift-pin test the way `common::submission_status` now pins its severity table
  to `broccoli_types::types::Verdict`.

## Testing Guidance

- Prefer the narrowest relevant command first.
- For backend changes, start with crate-scoped tests such as
  `cargo test -p server`.
- For frontend changes, at minimum run the affected package build or lint step.
- If you touch plugin build flows, consider using the CLI-backed plugin build
  recipes from the `justfile`.

## Practical Advice For Agents

- Read the owning crate or package before editing.
- Do not assume `CLAUDE.md` is fully up to date; verify against manifests and
  entrypoints when details matter.
- Check `Cargo.toml`, package manifests, and `justfile` before writing docs or
  instructions.
- When the repo and the guidance diverge, trust the repo and update the
  guidance.
