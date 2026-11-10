# Dedup backlog

Deferred extractions from the July 2026 duplication audit. Each entry is a
known, _aware_ copy pair — the second author knew about the first — left in
place because extraction was judged non-trivial at the time. When you touch one
of these areas, prefer doing the extraction over adding a third copy.

Done already (for reference on the pattern):

- `services/windowed_session.rs` — shared windowed-session engine extracted from
  `operation_batch.rs` / `evaluate_batch` (commit e618f98); a later
  drain-on-refill-failure fix then landed once instead of twice.
- `entity/judgement_reset.rs` — single source for the judged-output column clear
  list used by `dispatcher/steal.rs`, `dlq/stuck/recovery.rs`, and
  `services/submission_dispatch.rs` (whose pre-extraction judgement reset was
  missing `CompileOutput`).
- `Verdict::severity` — canonical table now lives only in
  `broccoli_types::types::Verdict`; `common::Verdict` delegates and carries a
  drift-pin test.
- `plugin-core/src/http.rs` — the `PluginHttpAuth`/`PluginHttpRequest`/
  `PluginHttpResponse` proxy DTOs now re-export `broccoli_types::types::*`
  instead of redefining them verbatim, so the host proxy and the guest SDK share
  one type (the old pair serialized compatibly only by hand — a silent
  serde-drift trap).
- `broccoli_types::types::push_judge_sets` / `push_double_opt[_str]` — the
  judged `SET`-list builder is single-sourced behind a `SqlBindSink` trait; the
  guest SDK (`server-sdk/src/sdk/shared.rs`) and the host mirror
  (`server/src/host_funcs/submissions.rs`) share the builder while each keeps
  its own bind policy (guest scrubs NUL eagerly at bind time; host defers to the
  `sql::execute_on_pool` choke point). Golden tests pin the exact SET-list/arg
  contract against drift.
- `utils::blob::take_required_file` / `resolve_virtual_path` — the
  post-multipart required-file unwrap+validate and the `path`-else-filename
  virtual-path resolution are single-sourced out of
  `handlers/{additional_file,attachment, config_upload}.rs`. The multipart drain
  loops themselves stay per-handler (each consumes different sibling fields and
  `next_field()` is single-pass); only the shared post-loop steps were
  extracted. Unit tests pin the error strings + the blank-path fallback.
- `host_funcs::HostCall` / `read_json` / `write_json` - the Extism host-fn
  prologue (snapshot `UserData`, open the `host_fn` span) is single-sourced for
  all host fns, raw `Function::new` callbacks and `host_fn!` bodies alike;
  contexts live behind `host_data`'s `Arc`, so the prologue is a refcount bump
  and no host fn holds the shared `UserData` mutex across its blocking call.
- `evaluate_batch::EvaluateVerdictCtx` - the seven values the evaluate
  result-wait helpers shared (plugin id, batches, metrics, ops registry, batch
  id, verdict channel, pending count) are one borrowed context; the helpers are
  its methods, and their three `#[allow(clippy::too_many_arguments)]` are gone.
- `services/batch_wait.rs` - one result-wait loop for operation and evaluate
  batches, replacing the four `next_{operation,evaluate}_result[_async]`
  skeletons. Tick receive, extend-while-pending, the give-up drain of a result
  buffered in the send-before-decrement race, outcome metrics and drained-batch
  cleanup exist once; each batch kind supplies a `WaitPolicy` (tick and ceiling,
  extension rule, delivery bookkeeping), and the blocking and async drivers
  differ only in their receive call. Supersedes the `EvaluateVerdictCtx` struct
  above.

## Open items

1. **Server test-support crate** — biggest single duplication.
   `packages/server/tests/e2e/common/mod.rs` (~1,291 lines) is a fork of
   `packages/server/tests/integration/common/mod.rs` (~1,265 lines):
   testcontainers Postgres/Redis singletons, admin-DB runtime workaround,
   AppConfig/AppState builders, DB-per-test locking, user/role seeding. The e2e
   copy has since gained Redis + worker wiring the integration copy lacks;
   harness fixes must be made twice. Fix: a dev-dependency `test-support` crate
   (cargo's per-tree test binaries are why the fork happened; a shared crate is
   the sanctioned way around that).

2. **Parametrized sandbox test harness**.
   `packages/worker/tests/worker_mock_sandbox.rs` (1,539 lines) and
   `worker_isolate_sandbox.rs` (898 lines) were cloned from each other on the
   same day and have drifted in coverage (newer cases exist only in the mock
   suite). A harness generic over `SandboxManager` restores parity.

3. **Shared API-client auth/retry protocol** (defensible split, lowest
   priority). `packages/cli-core/src/client.rs` (sync ureq) vs
   `packages/stress-test/src/client.rs` (async reqwest) both maintain login +
   token storage + retry-on-401 against the same server API.
