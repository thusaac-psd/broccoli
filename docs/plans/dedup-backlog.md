# Dedup backlog

Known copy pairs that stay in place because extracting them is not worth it
yet. Each entry names both copies and what would make extraction worthwhile.
When you touch one of these areas, extract instead of adding a third copy. When
you leave a cross-file copy in place, add it here (see the duplication rules in
`AGENTS.md`).

## Server test fixtures

`packages/test-support` holds the infrastructure both server test harnesses
share: the per-process Postgres and Redis containers, the migrated and seeded
template database, per-test database cloning (`create_test_database`), pool
teardown and `TestResponse`. The app fixtures themselves are parallel: `TestApp`
(`packages/server/tests/integration/common/mod.rs`) and `E2eTestApp`
(`packages/server/tests/e2e/common/mod.rs`) each build an `AppConfig` and
`AppState` and carry near-identical request helpers and user and role seeding
methods (`create_user_with_role`, `create_user_with_permissions`, ...).

They stay apart because they wire up different things: the e2e fixture adds
Redis, the MQ and an in-process worker. To extract, move the seeding helpers to
`test-support` as free functions over `&DatabaseConnection` and have both
fixtures delegate.

## API-client re-authentication

Both Rust clients read and write the wire types from `packages/api-types`
(`broccoli-api-types`), so no request or response shape is duplicated. The "on
401, re-authenticate once and resend" loop is parallel:
`Client::authorized` in `packages/cli-core/src/client.rs` (sync, ureq) and
`send_request_with_retry` in `packages/stress-test/src/client.rs` (async,
reqwest). The re-authentication differs: cli-core asks its `Session`
(`packages/cli-core/src/session.rs`) to rotate a refresh token through
`/auth/cli-refresh` under a cross-process file lock, while stress-test logs in
again with a username and password. Sharing the loop across a sync and an async
HTTP stack needs a trait heavier than the ten lines it replaces. Extract it if
both clients move to one HTTP stack or one auth flow.
