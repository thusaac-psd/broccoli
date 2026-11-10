# Kernel audit, November 2026

Static analysis plus a manual review of the **kernel**, meaning everything
outside `plugins/`: `packages/server`, `worker`, `common`, `mq`, `plugin-core`,
`broccoli-types`, `server-sdk` and `cli-core`. The review concentrated on
problem, contest, submission and user management.

**What came out of it:** 24 distinct bugs, each reproduced by a test that fails
today (31 `#[ignore]`d test functions in all), one latent panic in dead code,
and 14 more findings that come with a proposed test. The rest of this document
lists every item with its location and the command that reproduces it.

## How to run the bug reproductions

Each confirmed bug has a test that asserts the **correct** behaviour. It fails
today and is marked `#[ignore = "BUG: ..."]`, so CI stays green while the bug is
still documented in code. When you fix a bug, delete its `#[ignore]` and the
test becomes a regression guard.

```bash
# All integration repros (needs Docker for testcontainers):
cargo test -p server --test integration audit_ -- --ignored

# One area:
cargo test -p server --test integration audit_auth -- --ignored

# Unit-level repros:
cargo test -p plugin-core --lib hook::tests -- --include-ignored
cargo test -p server --lib client_ip -- --include-ignored
```

| File                                                    | Area                                       |
| ------------------------------------------------------- | ------------------------------------------ |
| `packages/server/tests/integration/audit_auth.rs`       | auth, users, roles, idempotency cache      |
| `packages/server/tests/integration/audit_contest.rs`    | contests, participants, contest config     |
| `packages/server/tests/integration/audit_problem.rs`    | problems, test cases, ZIP upload           |
| `packages/server/tests/integration/audit_submission.rs` | rejudge, epochs, retry budget, idempotency |
| `packages/server/tests/integration/audit_pagination.rs` | list endpoints (5 handlers)                |
| `packages/plugin-core/src/hook.rs` (tests module)       | hook output interpretation                 |
| `packages/server/src/client_ip.rs` (tests module)       | X-Forwarded-For parsing                    |

## Part 1: Static analysis

### Gates CI already runs (all clean)

| Check                                                | Result                                                                    |
| ---------------------------------------------------- | ------------------------------------------------------------------------- |
| `cargo clippy --workspace --all-targets -D warnings` | clean                                                                     |
| `cargo fmt --all --check`                            | clean                                                                     |
| `pnpm lint` (ESLint)                                 | 0 errors, 36 TSDoc-syntax warnings (plugin frontends)                     |
| `pnpm typecheck`                                     | clean, but only after `pnpm --filter @broccoli/web-sdk build` (see below) |
| `pnpm test` (node test runner)                       | 182/182 pass, again only after the web-sdk build                          |

Because CI already gates on default clippy, the default lint set finds nothing.
I ran an extra set of opt-in lints that target bugs over the kernel crates
(`--lib --bins`):

| Lint                                                |         Hits | Triage                                                                                                                                                                                                                                                                                    |
| --------------------------------------------------- | -----------: | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `arithmetic_side_effects`                           |          226 | Mostly counters. Led to the confirmed pagination overflow (B9).                                                                                                                                                                                                                           |
| `indexing_slicing`                                  |          106 | Noise at this volume; not triaged one by one.                                                                                                                                                                                                                                             |
| `map_err_ignore`                                    |           70 | Error context is dropped. Hygiene only.                                                                                                                                                                                                                                                   |
| `let_underscore_must_use`                           |           57 | Server hits reviewed: channel sends to dropped receivers, `OnceLock::set`, best-effort temp-file cleanup. All benign.                                                                                                                                                                     |
| `significant_drop_tightening`                       |           46 | Locks held a little longer than necessary. Performance, not correctness.                                                                                                                                                                                                                  |
| `cast_possible_wrap` / `_truncation` / `_sign_loss` | 44 / 32 / 19 | Reviewed. DB-sourced or bounded values, so benign on 64-bit.                                                                                                                                                                                                                              |
| `iter_over_hash_type`                               |           28 | Non-deterministic ordering. One case matters: `update_translations` (P1).                                                                                                                                                                                                                 |
| `string_slice`                                      |            8 | 7 provably safe. 1 latent panic in **dead code**: `utils/filename.rs::extract_stem` computes the stem end from the raw path length, while `Path::file_name()` normalises a trailing `/` or `/.`. For example `"a.é/."` slices in the middle of a character. It has no callers; delete it. |
| `unchecked_time_subtraction`                        |            2 | Guarded by an earlier `elapsed >= ceiling` check. Safe.                                                                                                                                                                                                                                   |
| `unwrap_in_result`                                  |            7 | `cli-core/model.rs`, `plugin-core/pool.rs`, `worker/models/worker.rs`. Low risk.                                                                                                                                                                                                          |

**Takeaway:** the kernel is clean at the lint level. Its bugs are logic bugs
(authorization, state machines, fencing), which is why most of the confirmed
findings below came from manual review backed by tests.

### Toolchain and environment findings

- **Two Redis client stacks per process.** The vendored `broccoli_queue` and
  `bb8-redis` use `redis 0.27.6`, while `common`, `mq`, `server` and `worker`
  use `redis 1.0.5`. Both versions are flagged future-incompatible, as is
  `proc-macro-error2`.
- **The build fetches from GitHub at compile time.** `utoipa-swagger-ui`'s build
  script downloads Swagger UI from github.com. Offline or proxied builds fail
  with `InvalidArchive("Could not find EOCD")`. The workaround is
  `SWAGGER_UI_DOWNLOAD_URL=file://...`; the cleaner fix is the crate's
  `vendored` feature.
- **The web typecheck depends on build order.** `pnpm typecheck` and `pnpm test`
  fail with about 30 `TS2307 Cannot find module '@broccoli/web-sdk/*'` errors
  until `web-sdk` is built. Make `typecheck` depend on the SDK build.
- **Nightly ICE.** `clippy::large_futures` crashes `clippy-driver`
  (`nightly-2026-08-23`) with an `unexpected rigid alias in layout_of` internal
  compiler error on `redis` async fns. Do not enable that lint until the
  toolchain is bumped.
- **No dependency advisory scan.** `cargo audit` could not run here, because the
  RustSec database lives on GitHub. Add `cargo audit` or `cargo deny` to CI.
- **The e2e suite needs a prior plugin build.** With no `plugins/*/*.wasm`, 49
  of 186 `tests/e2e` cases fail in the harness with `Wasm file not found`. This
  is not a code defect: CI runs `./scripts/build-plugins.sh` first. Run that
  locally before `cargo test -p server`, or the e2e binary fails and cargo never
  reaches the integration binary. The same applies to the `codelink_bracket*`
  integration tests (49 cases), which copy the built `codelink_bracket.wasm`.
- **One flaky test.** In a full integration run with `--test-threads=4`,
  `display_name::legacy_database_upgrade_and_restarts_preserve_account_names`
  failed once: a login after the in-test `Migrator::down` and `init_db`
  returned 500. It passed 3 of 3 runs in isolation. My unconfirmed guess is that
  a pooled connection's sqlx prepared-statement cache goes stale after the
  in-test schema change (Postgres "cached plan must not change result type").
  The server log was not captured, so the cause is not proven.

## Part 2: Confirmed bugs (each has a failing test)

Severity reflects impact on a live contest. "Test" names the `#[ignore]`d repro.

### High

**B1. Hidden contest problems can be judged outside the contest.** Two reviewers
found this independently. `POST /problems/{id}/submissions` authorizes
`Action::Submit` with the _read_ rule, because
`visibility/host_rules.rs::host_decide` ignores `_action`.
`decide_standalone_problem_access` rule 9 admits a hidden problem when it is
attached to a started contest the caller participates in. The row is stored with
`contest_id = NULL`. As a result:

- contest-scoped submission-limit and cooldown are skipped;
- IOI and ICPC feedback masking is skipped, because both only mask when
  `contest_id.is_some()`;
- the attempt never appears on the standings.

Only codelink-bracket blocks this, and it does so in its own hook. Test:
`audit_contest::hidden_contest_problem_rejects_standalone_submission`. Today the
request returns `201`.

**B2. `/auth/cli-token` turns revoked tokens into fresh ones.**
`handlers/auth.rs::issue_cli_token` (and `authorize_device`) take the stateless
`AuthUser` and re-sign the _old_ claims with `iat = now`. The new token passes
`FreshAuthUser`, and the endpoint also mints a new 7-day refresh token. That
defeats role revocation, password-reset session revocation and soft-delete.
Example: an admin whose role was revoked exchanges the stale token, then
re-grants themselves admin. Tests:
`audit_auth::cli_token_refuses_a_stale_access_token` and
`cli_token_refuses_a_deleted_user`. Both get `200` with a new token today.

**B3. No privilege ceiling on `update_user` or `delete_user`.** `assign_role`
and `revoke_role` enforce `require_role_within_ceiling`, but
`handlers/user.rs::update_user` and `delete_user` only check `USER_MANAGE`. A
principal with only `user:manage` can reset an admin's password and log in as
that admin, or delete every admin. Tests:
`audit_auth::user_manager_cannot_take_over_an_admin` (gets 200) and
`user_manager_cannot_delete_an_admin` (gets 204).

### Medium

**B4. A plugin's mistyped `reject` fails open.**
`plugin-core/src/hook.rs::interpret_hook_output` maps _any_ `HookResponse`
deserialization failure to `Pass`. That arm was meant for JSON with no `action`
key, but it also catches `{"action":"reject","status_code":"429"}`. So a
blocking gate such as cooldown or submission-limit lets the submission through.
Test: `hook::tests::mistyped_reject_fails_closed`. A companion test,
`mistyped_reject_currently_passes`, pins today's behaviour; delete it along with
the fix.

**B5. Rejudge epochs collide.** Two reviewers found this independently. A
deferred rejudge creates its judgement at `sub.judge_epoch + 1` without
advancing `sub.judge_epoch`, so the next immediate rejudge reuses that epoch.
The epoch is the fencing token, so a dispatch failure in the deferred run marks
the _live_ judgement as SystemError, and the `after_judging` dedupe key
collides. Test: `audit_submission::rejudge_epochs_are_unique`. The epochs come
out as `[11, 12, 12]`.

**B6. Rejudging an in-flight submission strands the old judgement.**
`open_rejudge_judgement` flips `is_current` but never finalizes the old
judgement. Its own finalize is then rejected, so the stuck detector re-runs it
up to 5 times and finally creates a DLQ entry, for every affected submission in
a bulk rejudge. Test:
`audit_submission::rejudge_finalizes_the_superseded_inflight_judgement`.

**B7. Plaintext passwords are kept in the idempotency cache.**
`middleware/idempotency.rs` stores the full 2xx response body. The user and
participant creation responses contain generated passwords, and the web client
sends `Idempotency-Key` on exactly those calls. The passwords sit in
`idempotency_key.response_body` for 24 hours, readable by the plugin DB role.
Test: `audit_auth::created_user_password_is_not_persisted_by_idempotency_cache`.

**B8. Idempotency keys ignore the request body.** Reusing a key with different
content silently replays the first response. Example: a contestant edits their
code after a network error, resubmits, and gets back the _old_ submission. Test:
`audit_submission::idempotency_key_reuse_with_different_body_is_rejected`.

**B9. Pagination overflow on five list endpoints.** `(page - 1) * per_page` is
computed on a caller-supplied `u64` in `list_problems`, `list_contests`,
`list_submissions`, `list_contest_submissions` and `DlqService::list`. With
debug assertions the handler panics and the connection drops. In release the
value wraps and returns the wrong page, or Postgres rejects an `OFFSET` above
`i64::MAX` with a 500. `submission/response.rs` already has the `checked_mul`
fix; it was never copied to the other five sites. Tests: `audit_pagination::*`.
All five fail with `attempt to multiply with overflow` at the expected lines.

**B10. Test-case writes buffer up to 1 GiB before checking permission.**
`require_permission(PROBLEM_EDIT)` runs after `AppJson` or `BaseMultipart` has
read the whole body. Any logged-in user can force large allocations.
`checker_source.rs` documents this class of bug and fixes it only for itself.
Test: `audit_problem::test_case_writes_check_permission_before_reading_body`. A
contestant gets 400 instead of 403.

**B11. Upload with `strategy=replace` returns 500 on a problem that has been
judged.** It deletes test cases without the `test_case_result` guard that single
and bulk delete have, so it hits the foreign key. Test:
`audit_problem::replace_upload_on_judged_problem_is_a_conflict_not_500`.

**B12. Auto-generated test-case labels collide.** The auto label is
`position.to_string()`, and after a normal 1-based ZIP upload that label is
always taken. The server then returns 409 for a label it generated itself. Test:
`audit_problem::create_test_case_after_zip_upload_gets_a_free_label`.

**B13. `submission_format` is validated after trimming but stored raw.** A
format that passes validation, such as `{"cpp": [" solution.cpp"]}`, makes the
problem impossible to submit to. Test:
`audit_problem::submission_format_with_padded_filename_is_normalised_or_rejected`.

**B14. `GET /contests` always returns `contest_type: null`.** The column is
missing from `select_only()`, and sea-orm maps a missing column to `None`
without an error. Test: `audit_contest::list_contests_includes_contest_type`.

**B15. Participants can unregister after a contest ends.** They then vanish from
the final standings. `register_for_contest` and `delete_user` both guard the
contest window; `unregister_from_contest` does not. Test:
`audit_contest::cannot_unregister_after_contest_end`.

**B16. `update_user` skips validation.** A taken username returns 500 instead of
409 `USERNAME_TAKEN`. Values such as `password: "x"` and `username: " padded"`
are accepted. A padded username can never log in, because login trims its input.
Tests: `audit_auth::update_user_to_taken_username_is_a_conflict` and
`update_user_validates_like_registration`.

**B17. Revoking a built-in role permission is undone on restart.**
`seed_role_permissions` re-inserts the defaults on every boot. Confirm the
intended policy: either keep the revoke or reject edits to built-in roles. Test:
`audit_auth::revoked_builtin_permission_survives_reseed`.

### Low

**B18. Assigning a role the user already holds returns 500.** It inserts a
duplicate primary key, and every new user already holds `contestant`. Test:
`audit_auth::assigning_an_already_held_role_is_not_a_server_error`.

**B19. Overlapping ZIP formats such as `*` and `*.a` (Polygon layout)
misclassify answer files as inputs.** Test:
`audit_problem::zip_upload_with_overlapping_formats_pairs_files`.

**B20. A soft-deleted problem's test cases can still be read through
`get_test_case`.** The list endpoint correctly returns 404. Test:
`audit_problem::soft_deleted_problem_hides_individual_test_cases`.

**B21. Contest-problem config can still be written after the contest is
soft-deleted.** The upsert re-creates exactly the rows the delete purged. Test:
`audit_contest::deleted_contest_rejects_contest_problem_config_writes`.

**B22. `retry_count` is never reset on rejudge.** The dispatch and stuck budgets
become lifetime counters, so after about 4 rejudges the first lease steal goes
straight to SystemError plus a DLQ entry. Test:
`audit_submission::rejudge_resets_retry_budget`.

**B23. The documented 10,000-ID cap on bulk rejudge is not enforced.** Above
65,535 IDs the request hits the Postgres bind-parameter limit and returns 500.
Test: `audit_submission::bulk_rejudge_enforces_documented_cap`.

**B24. An unparseable rightmost X-Forwarded-For hop falls back to a
client-supplied entry.** Example: a proxy that appends `ip:port`. The login
throttle then keys on a spoofed IP. The governor key extractor in `routes/v1.rs`
is a separate, divergent copy of this parser that is not listed in the dedup
backlog. Test:
`client_ip::tests::unparseable_rightmost_hop_does_not_trust_client_supplied_entries`.

**B25. Latent panic in dead code:** `utils/filename.rs::extract_stem`, from the
static-analysis section. The recommendation is to delete it rather than test it.

## Part 3: Proposed test cases (findings not yet reproduced)

These were traced in code but need richer fixtures (Redis, a worker, a plugin,
concurrency) than the HTTP harness provides. Each comes with the test that would
confirm it.

| #   | Sev     | Finding                                                                                                                                                                                                            | Location                                                                | Proposed test                                                                                                                                   |
| --- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| P1  | med     | `update_translations` clears the i18n registry and then returns early at the first bad TOML file. `disable_plugin` is left half-done: unloaded in memory, timers still pending, DB still saying `is_enabled=true`. | `plugin-core/src/traits.rs`, `handlers/admin.rs`, `utils/plugin.rs`     | Two loaded mock plugins, one with a missing translations path. After `update_translations()`, assert the good plugin's locale is still present. |
| P2  | med     | Registry keys are last-writer-wins across plugins. Plugin X can take over the "ioi" contest type. Purging X then leaves the key unregistered.                                                                      | `host_funcs/registry.rs`, `utils/plugin.rs::purge_plugin_registrations` | Insert "ioi" for plugin A and register it for B. Assert an error, or that A is kept. After `purge(B)`, assert "ioi" still resolves.             |
| P3  | high\*  | Host-cancelled ops are acked by the worker without a `TaskReply`, so each cancelled test case holds a fan-out permit for 30 minutes. \*Only when `cancel_primitive_enabled=true`.                                  | `worker/src/consumer.rs`, `services/evaluate_batch/*`                   | Extend `redis_cancel_keys_preempt_execution` to assert a `cancelled_by_host()` reply is published.                                              |
| P4  | high    | A worker that restarts with the same static `worker_id` strands its in-flight ops, because the reaper skips owners that have a live heartbeat.                                                                     | `dispatcher/operation_reaper.rs`                                        | Seed a `_processing` uuid owned by w1 and a live w1 heartbeat whose `started_at` is later than the claim. Assert it is requeued.                |
| P5  | med     | When a server restarts with lease/steal disabled (the default), `recover_orphaned_leases` is skipped and Running rows hang for about 1 hour.                                                                       | `runtime.rs`, `dlq/stuck/mod.rs`                                        | Seed a Running row owned by `server-1`, boot as `server-1` with steal off, run one detector scan, and assert it is re-dispatched.               |
| P6  | med     | A cancelled detached-evaluate slot still dispatches its ops and re-creates an `EvaluateBatchOps` entry that is never removed. This is unbounded growth, and every reply triggers an O(n) scan.                     | `host_funcs/evaluate_ops_registry.rs`                                   | `remove_batch("eval-1")`, then `start_operation_batch` with ops tagged `eval-1`. Assert nothing is published and nothing is recorded.           |
| P7  | low     | A second stuck-exhaustion of the same judgement is dropped if an operator resolved the first DLQ entry, because of the deterministic id plus `ON CONFLICT DO NOTHING`.                                             | `dlq/stuck/handlers.rs`                                                 | Pre-insert a resolved `stuck-submission-judgement-N`, exhaust again, and assert an unresolved entry exists.                                     |
| P8  | low-med | Race: `delete_problem` and `add_contest_problem` can both succeed, because the add path reads the problem without a lock.                                                                                          | `handlers/contest/problems.rs`                                          | Hold `FOR UPDATE` on the problem in a raw transaction, start the add, soft-delete and commit, then assert the add does not return 201.          |
| P9  | low     | Race: a private reply's text can leak through the legacy `reply_content` field, since the last writer wins and the redaction is keyed on the newest reply.                                                         | `handlers/clarification.rs`                                             | Build the post-race state in the DB. Assert a non-participant does not see the private text.                                                    |
| P10 | low     | Plugin config upsert never validates against the manifest schema or the declared scopes.                                                                                                                           | `services/plugin_config.rs`                                             | Upsert config that breaks a manifest `min` or `type`, and config for an undeclared scope. Expect 400.                                           |
| P11 | low     | `store_scan` returns an unordered map and its cursor depends on DB collation, so pages can skip or repeat keys on non-C locales.                                                                                   | `host_funcs/storage.rs`                                                 | Write more than 100 mixed-case keys and assert the pages walk them completely without overlap.                                                  |
| P12 | med     | Refresh rows are never compared with `credentials_changed_at`. A `/auth/refresh` that holds the row lock during a password reset inserts a successor row the reset never sees.                                     | `utils/refresh.rs`, `handlers/auth.rs`                                  | Mint a refresh token, bump `credentials_changed_at`, call refresh, and expect 401.                                                              |
| P13 | low     | Unregistering from a contest is an existence oracle: a missing contest gives "Contest not found", a private one gives "Not registered".                                                                            | `handlers/contest/participants.rs`                                      | DELETE `/contests/{private}/register` as a stranger and assert the same 404 body as for a missing id.                                           |
| P14 | low     | Username uniqueness rests on a partial unique index created warn-only in `seed::ensure_indexes`. The original constraint was dropped in m0001.                                                                     | `seed.rs`, `migration/m0001_*`                                          | Assert the index exists after boot, and fail boot if it cannot be created.                                                                      |

## Part 4: Design concerns (not bugs today, but risky)

- **The plugin DB role can read every core table.** m0003 re-granted SELECT on
  all tables, including `user`, `refresh_tokens`, `idempotency_key` (see B7),
  and other plugins' `plugin_config` and `plugin_storage`. The comments in
  `host_funcs/mod.rs` that say otherwise are out of date. The SQL guard was not
  fully audited (the reviewer was cut off), so it deserves a dedicated pass.
- **One global hook can bypass other gates.** A `Stop` from a Global
  `before_submission` hook accepts the submission and skips every
  resource-scoped gate after it, including the contest owner's submission-limit
  and cooldown.
- **The contest-window predicate is hand-copied six or more times.** This breaks
  the AGENTS.md rule of three, and there is no drift-pin test. `delete_user`
  uses yet another definition of "active".
- **List endpoints have no unique ORDER BY tiebreaker.** Sorting on contests,
  problems, `contest_problem.position` and participants by `registered_at` is
  unstable on ties, so OFFSET paging can skip or repeat rows.
- **Clocks are mixed.** Lease heartbeats use DB `NOW()` in claim/steal and app
  `Utc::now()` elsewhere, and `FreshAuthUser` compares the app clock with DB
  `now()`. Skew between hosts can cause steal ping-pong or re-login loops.
- **No blob garbage collection.** Blobs from skipped, aborted or replaced
  uploads are never collected, although the delete docs say "preserved for GC".
- **The reply-queue sweeper is effectively dead.** Nothing writes
  `broccoli:server:heartbeat:<id>`, so `sweep_once` always fails closed.
- **The SE-retry reaper (budget 50) hides the 5-attempt dispatch and stuck
  budgets**, and with them several of the bugs above (B5, B22, P5). Budget
  semantics should be made explicit.

## Suggested fix order

1. **B1, B2, B3**: authorization holes that work on live contests and admin
   accounts today.
2. **B4, B7, B10**: a fail-open gate, plaintext credentials at rest, and a cheap
   memory DoS.
3. **B5, B6, B22**: the rejudge state machine. These share a root cause:
   judgement lifecycle and fencing are spread across rejudge, dispatch and
   consumers.
4. **B9**: one shared `checked_offset(page, per_page)` helper, which also
   removes the drift that caused the bug.
5. Everything else, opportunistically, deleting each `#[ignore]` as it lands.
