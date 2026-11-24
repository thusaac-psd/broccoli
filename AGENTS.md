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
- A node's default id has one rule, in the binary
  (`common::node_identity::default_node_id`, printed by `--node-id`). Installers
  never derive one: they ask the role's image and record its answer.

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

`third_party/extism` is extism 1.41.0 vendored as a `[patch.crates-io]`, with
fixes to how a call's deadline, cancel and store reset interact;
`third_party/extism/README.md` lists the changes. Edit it there, never in the
cargo registry.

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
  `frontend/` and optional `i18n/`.
- Every language of the host UI is a language pack: a plugin whose manifest
  declares `[language_pack] locale` (`broccoli-en`, `broccoli-zh-cn`). The packs
  are the only source of host strings, one enabled pack per locale (enabling a
  second answers `LANGUAGE_PACK_TAKEN`). An identifier's name is served only
  from the plugin it resolves to (`plugin_core::i18n::served_translations`). A
  plugin id is never a host key namespace
  (`plugin_core::i18n::is_host_namespace`, derived from the English pack by
  `plugin-core/build.rs`), so a plugin's own keys are never the host's.
  `packages/web/scripts/language-packs.ts` embeds the packs in the repo into the
  web build (`src/lib/i18n/packs.gen.ts`, generated by Vite, `pnpm typecheck`
  and `pnpm lint`) as the offline snapshot, and the message key type comes from
  the English pack. Packs the server serves override the embedded ones. Other
  plugins translate only their own keys.
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
cargo run -p worker               # just worker (builds broccoli-compare first)
```

### JavaScript

```bash
pnpm install                      # just install
pnpm build                        # just build-js
pnpm dev                          # just dev
pnpm --filter @broccoli/web dev   # just dev-web
pnpm --filter @broccoli/web-sdk build   # just build-sdk
pnpm lint                         # just lint-js (one eslint run over every TS project: a 4 GiB heap)
pnpm format                       # just format
pnpm format:check                 # just format-check
pnpm test
pnpm typecheck
pnpm --filter @broccoli/web test:e2e   # browser checks of the built app (pnpm build first)
```

### Plugins and other recipes

```bash
just build-plugins                # every plugin and server test fixture
just build-plugin plugins/ioi --install
just test-plugins                 # each plugin crate's tests
just clippy-plugins               # plugin crates for the target each ships as, and the server SDK guest build
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
`POSTGRES_DB`, `BROCCOLI_DB_USER`, `BROCCOLI_DB_PASSWORD` and `REDIS_PASSWORD`
override them. The server logs in as the app role `broccoli`, a non-superuser
that owns the database and holds `CREATEROLE` (it provisions each plugin's own
role); the superuser only initializes the database:
`postgres://broccoli:broccoli_app_secret@localhost:5433/broccoli` and
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
- A plugin's raw SQL (`host.db.*`) runs as that plugin's own database role, in
  its own schema, on its own pool (`packages/server/src/plugin_db`). Host tables
  are read-only to plugins, and content columns stay hidden unless the manifest
  declares a `[server.sql] read` scope. The app role provisions these roles, so
  it needs `CREATEROLE` but never superuser. Every plugin statement's answer
  passes one outage rule (`host_funcs::sql::settle`): an outage fails the call
  as unavailable and, under a dispatch, hands the attempt back.
- `packages/plugin-core` holds the plugin abstractions shared by the host:
  manifest parsing, the registry, instance pools, hooks, admission, config, i18n
  and the plugin HTTP proxy types.
- `packages/contestant-cli` and `packages/dev-cli` talk to the server through
  `packages/cli-core`. A request path names a contest or problem only by
  `broccoli_cli_core::ids::{ContestId, ProblemId}`, which come from the server's
  answers or from `broccoli_cli_core::resolve`, so text a user typed (a title, a
  label) never reaches a URL. Text a user typed reaches a request only as a
  query value (the contest search, `?search=`): the client percent-encodes every
  value (`ApiClient::get_query`), and the server matches it as data (an escaped
  `LIKE` pattern), never as a path, an id or a filter expression.
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
- Every error code the host sends or records is a variant of
  `broccoli_api_types::error::HostCode`, and every language pack has an
  `apiError.<CODE>` text for it unless the variant is marked `generic`;
  `packages/api-types/tests/host_code_text.rs` fails otherwise.
- An outage of something a request needs answers a retryable 503 with
  `Retry-After` through one `AppError` variant per dependency
  (`DATABASE_UNAVAILABLE`, `QUEUE_UNAVAILABLE`, `STORAGE_UNAVAILABLE`), each
  decided by that dependency's one outage rule (`common::db::is_outage`,
  `common::redis_conn::is_outage`); an answer no retry changes stays a 500.
  Every 503 carries `Retry-After`: a disabled plugin or contest type
  (`PLUGIN_DISABLED`, `CONTEST_TYPE_DISABLED`) is one too, since what needs it
  is parked and serves once an administrator enables it
  (`DISABLED_RETRY_AFTER_SECS`). Per-address quotas admit while Redis is down,
  so an outage never locks a venue out.
- A per-address quota counts what abuse leaves behind, never what a venue does:
  failed sign-ins, not sign-ins; device codes left pending, not device codes
  requested. A venue shows the server one address for all its contestants.
- Prefer permission helpers instead of checking roles directly.
- Think about transaction boundaries before editing write paths.
- The schema changes only through versioned forward migrations in
  `packages/server/src/migration`: a schema change is a new `mNNNN_<what>`
  migration appended to `Migrator::migrations`, never an edit to a shipped one.
  Boot applies pending migrations under the boot lock and refuses a database a
  newer build migrated. The test
  `migration::tests::the_migrations_create_the_schema_the_code_expects` fails
  when the entities or the code-owned trigger DDL disagree with the migrated
  schema. Old replicas keep working against an additive migration (adding a
  table, a nullable or defaulted column, an index), so a release of only those
  rolls server by server; the shipped gateways pin each browser to one server by
  cookie, so it never loads one build's page with another's chunks. A release
  with any other migration (a drop, a rename, a type change, a tighter
  constraint) is upgraded with every server stopped before the first new one
  starts, and says so in `release/docs/upgrade.md`; there are no deployments to
  keep an old shape for, so no expand/contract.
- When changing plugin execution behavior, check both manifest-level permissions
  and host-side registration/resolution.
- A unit of work runs only while every plugin it needs has an enabled provider:
  its contest type's plugin, its problem type's evaluator and its checker
  format's checker (`providers::Needs`). Work that cannot run because one is
  disabled fleet-wide is parked, not failed: the claim, the system-error retry,
  the stuck detector and the overview's parked count read one predicate
  (`judging::Unit::runnable`, built on `providers::runnable`;
  `PluginLifecycle::closed_by` in Rust), the submit paths refuse it
  (`accepting_handler`), and it resumes when a provider is enabled. A replica
  takes a unit only when it has every key's fleet-wide provider loaded, or no
  live replica has (`providers::runs_here`, local registrations from
  `Loaded::here`); a plugin lost after the claim hands the attempt back
  (`Dispatch::not_loaded_here`). A new scan that hands out or retries judging
  work uses these predicates too. A judgement whose session the disable cut goes
  straight back to `Queued` (`judging::fail_dispatch`), never through
  `SystemError`, on every replica alike, since the decision reads the fleet-wide
  `plugin` table.
- A worker that lacks a platform tool takes no work
  (`HeartbeatPayload::accepts_work`): it stays live, its heartbeat lists what is
  missing, and while no live worker takes work the claim claims nothing
  (`ClaimPass::CANNOT_RUN`), so submissions wait `Queued` instead of failing. A
  missing tool is the worker's fault, a hand-back that spends no retry; work
  already dispatched when the last capable worker stops taking work is handed
  back under `NO_CAPABLE_WORKER`.
- Every judgement write a plugin makes goes through one rule,
  `judging::as_recorded`: the plugin's error code is recorded as
  `<plugin_id>.<CODE>`, a terminal write of an attempt the platform lost is
  fenced out, and a system error under a dispatch fault carries the fault's bare
  code. `SystemErrorClass` matches only bare host codes, so a plugin can never
  write a code that hands back or stands; a new host code stays bare.
- A judgement or code run goes to the plugin that provides its contest type as a
  `services::judging::Dispatch`, which that call, its nested calls and the
  detached sessions it starts carry in their call locals. Every plugin write to
  a unit passes `Dispatch::owned` (the owning plugin, the dispatched unit)
  before the database fence sees it. A unit is live from its claim
  (`judging::LiveDispatches::enter`, before its dispatch can wait) while its
  entry or any clone of the dispatch built from it exists, and the lease refresh
  keeps only live units' leases, so work a server owns but lost lapses and is
  stolen.
- A failure every attempt would repeat stands: its `DispatchFault` classes as
  standing (a problem's checker or its own program failing, `CHECKER_FAILED`,
  `PROBLEM_FAILED`), and the system-error retry never re-judges it. An evaluator
  reports a problem's failure generically, in
  `TestCaseVerdict::problem_failure`; the host never reads a plugin's verdict
  for it. A system error the retry may still take back is not shown: the
  submission and its judgement history show the attempt in flight until the
  retry decides.
- Unloading a plugin retires its instance pool (`plugin_core::pool::Liveness`)
  and every pool a reload replaced that a call still runs on
  (`PluginEntry::succeed`): a call already running on a retired instance is
  refused at its next host function (`plugin_core::executor::instance_retired`),
  so nothing an unloaded plugin does lands. A registry entry is replaced only
  through `succeed` (the reload took) or `discard` (it did not).
- What a plugin says to a client passes `services::plugin_output`: its codes go
  out as `<plugin_id>.<CODE>` (`plugin_code`), its refusals only with a
  client-error status the host allows, and a route answers only a status and a
  JSON body, so the host writes every header (no cookie, redirect or page from a
  plugin). A plugin never speaks as the host or as another plugin.
- Several plugins may provide one key (a contest type, a problem type, a checker
  format, a language). `registry::Providers` keeps every registration. Judging
  work resolves a key to its fleet-wide provider, the enabled plugin providing
  it whose id sorts first (`PluginLifecycle::provider`, `providers::provider` in
  SQL), through `providers::resolve`, so every replica runs a key on the same
  plugin and a replica that lacks that plugin never falls back to another.
  Register and resolve through these, never through a map of your own.
- Every submit path (practice and contest submissions, code runs) accepts work
  through one rule, `plugin_lifecycle::accepting_handler`, which refuses a
  contest type no provider can judge now; a new submit path calls it too.
- A change to a problem's test data goes through
  `services::test_data::TestDataEdit`, which counts it in
  `problem.test_data_version` (judging copies the version onto every judgement
  it claims, and applying a judgement of another version needs `force`), and
  whose commit checks the problem's stored configs against the data as the
  change leaves it and refuses a change that breaks one with 409
  `CONFIG_WOULD_BREAK` unless the request says `force=true`. The problem's
  `config_issues` flag, which judging copies onto every judgement it claims, is
  kept by `services::config_issues`: a new path that changes what a stored
  config is checked against (test data, a plugin, a contest- or plugin-scope
  config) rechecks it there, after it commits and off the judging path.
- Every write or delete of a stored plugin config, an admin's, a plugin's
  (`config_set`) or the host's own as a problem or contest goes, goes through
  `services::plugin_config` (`write_config`, `delete_config*`): a write passes
  the namespace's schema and `validate` function before it is stored, and each
  returns the `ConfigChange` that rechecks the `config_issues` flags it reaches
  once it commits. Only a bundle import writes the table in bulk; the server's
  next start rechecks what it restored.
- A plugin route declares its access in `plugin.toml` (`access`, `permission`,
  `contest`, `problem`); the proxy resolves the route's contest or problem
  through the visibility kernel and checks the token's freshness before the
  plugin runs, so a handler reads `req.contest()` / `req.problem_id()` instead
  of checking access itself.
- A plugin's own words reach the UI in the user's language through its pack: a
  `validate` refusal carries a key under the plugin's id (`ioi.config.…`) with
  its params, and the host drops any other key so the message shows.
- The host owns no plugin's records: what it stores or serves for plugins it
  finds through a manifest declaration (for example the problem-scoped config
  namespace a checker plugin marks `checker_source`), never through a plugin id.

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
  Docker; plugin-loading tests need `just build-plugins` first. The e2e harness
  builds the worker's platform tools (`broccoli-compare`) into
  `target/e2e-tools` itself, so `E2E_SANDBOX_BACKEND=mock` is all a mock run
  needs. A submission reads `Judged` before its after-judging hooks run, so an
  e2e test that counts or inspects what judging did, its hooks' effects
  included, waits for `E2eTestApp::judging_idle`, which completes once every
  judging session and contest submission call on the server has finalized and
  run its hooks; `pause_worker` holds operations queued, and `reconcile_plugins`
  applies a change another replica published.
- Release shell tests (`release/test`, `release/*/test`) put `release/test/bin`
  first on `PATH`: its `docker` answers an image's `--node-id` question and
  passes everything else to the real engine.
- For frontend changes, run `pnpm lint`, `pnpm test` and the affected package's
  build or `pnpm typecheck`.
- For plugin changes, run `just test-plugins` (or
  `just test-plugin plugins/<name>`) and `just clippy-plugins`.
- For docs changes, run `cd website && pnpm build`; it fails on broken links in
  either locale.
- Before pushing, run what CI runs: `just check-all`, plus `cargo fmt` and
  `cargo doc` as above.
