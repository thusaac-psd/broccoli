# AGENTS.md

Guidance for coding agents working in this repository. When this file and the
repo disagree, trust the repo (`Cargo.toml`, the package manifests, the
`justfile`) and fix this file.

## Non-negotiables

- Never run `git stash`, `git reset --hard`, or other destructive commands
  unless the user explicitly asks for them.
- Preserve unrelated local changes.
- Prefer small, targeted edits over broad refactors.
- Validate with the smallest relevant command before finishing.

## Platform rules

- The host knows nothing about individual plugins. Server, worker, web app and
  SDKs never name a plugin, its routes, its tables or its verdicts; a plugin
  reaches the platform only through the generic hooks, host functions, slots and
  permissions every plugin gets.
- Fix the class of problem, not the symptom. When a bug has a cause that other
  code can hit too, fix the cause and pin it with a test or invariant check.
- No compatibility shims while there are no deployments. Change the schema, the
  wire format or a config key outright; do not keep the old shape alongside.
- Deterministic tests only. No sleeps to wait for state, no dependence on
  wall-clock timing, ordering of concurrent tasks or the network; drive time and
  ordering explicitly.
- Never compare wall clocks across nodes. Use `common::clock::PlatformClock`
  (the database clock) or Redis `TIME` for anything two nodes must agree on.
- Contests run on closed lab networks: no internet access and often no NTP.
  Release bundles carry everything they need: nothing at install or run time may
  fetch from the internet, and nothing may assume host clocks agree.
- Out of the box: operators set only secrets and URLs. Every other setting is
  derived from the machine and the request or has a working default.
- Comments and docs describe the code as it is now, in the present tense: the
  rule and the reason. No history ("used to", "previously", "the fix"), no
  commit SHAs, no task, review or plan labels.
- Worker concurrency stays 1 (`worker.max_concurrency`) so every submission is
  judged under the same load, for fairness. Scale by adding workers.

## Repo shape

Broccoli is a Rust and pnpm monorepo for an online judge with a WASM plugin
system.

### Rust workspace

Members of the root `Cargo.toml` workspace:

- `packages/api-types` (`broccoli-api-types`): the REST request and response
  types the server serializes and the Rust clients read.
- `packages/cli-core` (`broccoli-cli-core`): the CLI API client, credentials and
  session shared by both CLIs.
- `packages/common`: shared host types (verdicts, statuses, storage, the
  platform clock, the database helpers).
- `packages/mq`: the Redis Streams message queue.
- `packages/plugin-core`: the plugin runtime (manifest, registry, instance
  pools, hooks, host-function plumbing, plugin HTTP).
- `packages/server`: the axum API server.
- `packages/worker`: the judge worker. It consumes judging and operation tasks
  from the MQ and runs them in the isolate sandbox (Linux only).
- `packages/test-support`: the dev-dependency harness shared by the server's
  `tests/integration` and `tests/e2e` trees.
- `packages/contestant-cli` (`broccoli-contestant-cli`, the `broccoli` binary)
  and `packages/dev-cli` (`broccoli-dev-cli`, the `broccoli-dev` plugin tool).
- `packages/stress-test` and `packages/broccoli-compare`: members but not
  `default-members`, so build them with `-p <name>`.

Path dependencies outside the members list, published to crates.io:

- `packages/broccoli-types`: the types the guest SDK and the host share.
- `packages/server-sdk` (`broccoli-server-sdk`): the Rust SDK for backend WASM
  plugins.

Plugin crates under `plugins/` are excluded from the workspace. Each has its own
`Cargo.lock` and builds to `target/plugins`.

### JavaScript workspace

One pnpm workspace (`pnpm-workspace.yaml`) and one `pnpm-lock.yaml` cover:

- `packages/web-sdk` (`@broccoli/web-sdk`)
- `packages/web` (`@broccoli/web`)
- every plugin frontend, at `plugins/<plugin>/frontend`

Shared tool and library versions live in the `catalog:` of
`pnpm-workspace.yaml`. The documentation site under `website/` keeps its own
lockfile.

### Plugins

- Every plugin lives at `plugins/<plugin>/` with a `plugin.toml`, an optional
  Rust crate (`Cargo.toml`, `src/`) for the backend WASM module, an optional
  `frontend/` and optional `i18n/`. `broccoli-zh-cn` is manifest and locales
  only.
- The server's test fixture plugins live under `packages/server/tests/fixtures`
  and `packages/server/tests/fixtures_timer_negative`; `just build-plugins`
  builds them with the real plugins.

### Agent skills

The docs skills live in `.agents/skills/`. `.claude/skills` is a symlink to that
directory, so edit `.agents/skills/` only.

## Commands

Prefer the `justfile` recipes; each maps to the plain command shown.

### Rust

```bash
cargo build                       # just build (default members)
cargo test --workspace --exclude stress-test   # just test (builds plugins first)
cargo test -p server              # just test-crate server
cargo clippy --workspace --all-targets --locked -- -D warnings   # just clippy
cargo fmt --all -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo run -p server               # just server
cargo run -p worker               # just worker
```

### JavaScript

```bash
pnpm install                      # just install
pnpm build                        # just build-js
pnpm dev                          # just dev
pnpm --filter @broccoli/web dev   # just dev-web
pnpm --filter @broccoli/web-sdk build   # just build-sdk
pnpm lint                         # just lint-js
pnpm format                       # just format
pnpm format:check                 # just format-check
pnpm test
pnpm typecheck
```

### Plugins and other recipes

```bash
just build-plugins                # every plugin and server test fixture
just build-plugin plugins/ioi --install
just test-plugins                 # each plugin crate's tests
just clippy-plugins               # plugin crates and the server SDK guest build
just gen-api                      # regenerate web-sdk API types from OpenAPI
just test-sandbox                 # isolate suites (needs isolate, cgroup v2)
just e2e-local                    # server e2e in-process with the mock sandbox
just check-all                    # the CI checks for Rust and JS
cd website && pnpm build          # docs site, English and zh-CN
```

### Infrastructure

```bash
docker compose up -d              # just up
docker compose down               # just down
```

`docker-compose.yaml` runs, bound to loopback:

- Postgres 17 on `127.0.0.1:5433`
- Redis 7 on `127.0.0.1:6379`
- SeaweedFS on `127.0.0.1:9333` (master) and `127.0.0.1:8333` (S3)

It needs no setup. The credentials default to the ones
`config/config.example.toml` uses, and `POSTGRES_USER`, `POSTGRES_PASSWORD`,
`POSTGRES_DB` and `REDIS_PASSWORD` override them:
`postgres://postgres:broccoli_pg_secret@localhost:5433/broccoli` and
`redis://:broccoli_redis_secret@localhost:6379`.

### Configuration

Settings are required (URLs, secrets), derived (pool sizes, the claim loop,
cookie security, log format) or advanced overrides with defaults, listed in
`release/docs/configuration-reference.md`. Do not add a key whose value can be
derived from the machine or the request; a removed key needs no shim, since the
binaries warn about every key they do not read.

## Architecture notes

### Backend

- `packages/server` is the axum server: handlers, the visibility kernel, the
  plugin host functions and the judging services.
- `packages/worker` consumes MQ tasks and operation tasks in a reconnect loop.
- `packages/plugin-core` holds the plugin abstractions shared by the host:
  manifest parsing, the registry, instance pools, hooks, admission, config, i18n
  and the plugin HTTP proxy types.
- `packages/contestant-cli` and `packages/dev-cli` talk to the server through
  `packages/cli-core`.
- `packages/server-sdk` and `packages/broccoli-types` are the plugin-facing Rust
  API: change them together with the host side they mirror.

### Frontend

- `packages/web` uses React 19, React Router 7 in framework mode (SPA,
  `ssr: false`) on Vite 8, Tailwind, TanStack Query, Monaco and Radix UI.
- Routing is file-system based through `@react-router/fs-routes`, with
  `packages/web/src/routes.ts` adding an explicit catch-all extension route.
- `packages/web-sdk` is a published TypeScript package built with `tsdown`; the
  web app and plugin frontends build against it.
- Unit tests are `*.test.ts` files run by `pnpm test` (`node --test` with type
  stripping), so a tested module may use the `@/` alias only in `import type`;
  runtime imports must be packages or relative `.ts` paths.

## Working rules

### Backend changes

- Keep handler flow consistent: docs, tracing, auth, validation, DB/business
  logic, response.
- Use the project's structured error patterns and treat error `code` values as
  API contracts.
- Prefer permission helpers instead of checking roles directly.
- Think about transaction boundaries before editing write paths.
- The server only boots on an empty database or one this build created:
  `build.rs` stamps the schema sources (entities, migrations, boot-time trigger
  DDL) and boot refuses any other stamp. Keep schema DDL in those files so the
  stamp covers it.
- When changing plugin execution behavior, check both manifest-level permissions
  and host-side registration/resolution.

### Frontend changes

- Keep changes aligned with the existing React Router 7 file-based structure.
- Do not describe the frontend as "plain Vite React" in docs or code comments;
  this package uses React Router's framework tooling on top of Vite.
- Preserve the existing Tailwind/Radix/component patterns already in
  `packages/web`.

### SDK and API surface changes

- If you change server request/response shapes, check whether
  `packages/server-sdk` and `packages/web-sdk` need to move with them. A type a
  Rust client reads belongs in `packages/api-types`, never in a client-side
  copy. Run `just gen-api` to regenerate the web SDK's API types.
- Be careful with exported package entrypoints in `packages/web-sdk`, since its
  `package.json` exposes multiple subpaths, and plugin bundles resolve them
  through `SDK_SHARED_DEPS` in `packages/web/plugins/shared-deps.ts`.

### Duplication discipline

The duplication that drifts is the deliberate copy: a block copied from a file
the author knew about. Two copies of a table can rank the same value differently
on host and guest and nothing notices. Rules:

- Rule of three: when you are about to write the third copy of a pattern,
  extract it instead. If a boundary blocks extraction (WASM guest vs host,
  cargo's per-tree test binaries), say so in a comment at the copy site and add
  the pair to `docs/plans/dedup-backlog.md`.
- Never copy a block of ~20+ lines between files without either extracting it or
  leaving a comment that links the copies and an entry in the dedup backlog.
- Before adding a helper, grep for an existing one. Reviewers flag hand-rolled
  duplicates of shared utilities as blocking findings.
- Cross-boundary mirrors (guest SDK vs host) live in `broccoli-types` when
  possible. If a verbatim mirror is unavoidable, both sides carry a drift-pin
  test the way `broccoli_api_types::status` pins its severity table to
  `broccoli_types::types::Verdict`.

## Testing guidance

- Run the narrowest relevant command first: `cargo test -p <crate>`, or one test
  file with `cargo test -p server --test integration <name>`.
- Server tests start Postgres and Redis through testcontainers, so they need
  Docker; plugin-loading tests need `just build-plugins` first.
- For frontend changes, run `pnpm lint`, `pnpm test` and the affected package's
  build or `pnpm typecheck`.
- For plugin changes, run `just test-plugins` (or
  `just test-plugin plugins/<name>`) and `just clippy-plugins`.
- For docs changes, run `cd website && pnpm build`; it fails on broken links in
  either locale.
- Before pushing, run what CI runs: `just check-all`, plus `cargo fmt` and
  `cargo doc` as above.
