---
title: Plugin database access
sidebar_label: Plugin database access
sidebar_position: 6
---

# Plugin database access

A plugin with the `sql` permission runs SQL through `host.db.*`
(`packages/server/src/host_funcs/sql.rs`). Postgres decides what that SQL may
do: every plugin logs in as a role of its own, owns one schema, and can read a
curated set of host columns. Host state changes only through host functions,
which carry the guards the host relies on (the judging epoch fences and the
finalized guard, DLQ ownership, the `plugin_config` change counter, accounts
and tokens).

## What a plugin may do

| Object | Privilege |
| --- | --- |
| Its schema `plugin_<id>` | Owner: full DDL and DML, `TRUNCATE`, foreign keys between its own tables |
| Contest data in `public` (`PLUGIN_READABLE` in m0001) | `SELECT` on every column but the content columns |
| A content column its manifest declares | `SELECT` on that column |
| `"user"` | `SELECT` on `id`, `username`, `display_name`, `created_at`, `deleted_at` |
| Every other host relation | Nothing |
| Host sequences, schema `public` | Nothing; `CREATE` on `public` is revoked from `PUBLIC` too |
| Another plugin's schema | Nothing |

The readable relations are contest data with no secret in them: contests,
problems, test cases, submissions, judgements, results, code runs,
clarifications and problem files. Unreadable: password hashes and the
credential timestamp, refresh tokens, device codes, idempotency keys and
bodies, the plugin login secrets, the blob store, roles and permissions, the
plugin registry, every plugin's host-held state (`plugin_config`,
`plugin_storage`, `plugin_timer`, `plugin_replica`, `submission_feedback`), the
DLQ, and the schema stamp and migration record.

Writes to host state go through host functions: `host.submission.*` and
`host.code_run.*` for judgements and results, `host.storage.*`,
`config:read`/`config:write`, `host.timer.*` and the submission feedback
functions. None of the shipped plugins writes a host table directly.

## Content columns and read scopes

`SQL_READ_SCOPES` in `packages/broccoli-types/src/manifest.rs` lists the host
content columns, as `table.column`: contestant source (`submission.files`,
`code_run.files`), compiler and program output, test data
(`test_case.input`, `expected_output`, their previews and blob hashes),
checker output, custom tests, problem statements and clarification bodies. No
plugin reads them by default. Evaluators get test data through
`host.submission.query_test_cases`, which runs on the host function pool.

A plugin that needs one declares it:

```toml
[server]
permissions = ["sql"]

[server.sql]
read = ["submission.files"]
```

The manifest fails to parse on a scope outside the list, on an unknown key
under `[server.sql]`, and on `read` without the `sql` permission. The upload
dialog and the plugin detail view show the declared scopes beside the
permissions, so the admin approves them with the plugin. Provisioning revokes
every content column from the plugin's role and grants exactly the declared
ones, and runs again when a new version declares others. The print plugin
declares `submission.files` to print a submission's source.

## Roles and schemas

`packages/server/src/plugin_db/schema.rs` derives every name from the plugin
id and the database, so the host holds no per-plugin knowledge:

- the schema is `plugin_<id>`;
- the database's group role is `broccoli_<db>_plugin`, and each plugin's login
  role `broccoli_<db>_plugin_<id>`, where `<db>` is the first 8 hex digits of
  the sha256 of the database name, because roles are shared by every database
  of a cluster;
- an id too long for Postgres' 63-byte identifiers keeps its head and ends in
  a digest of the whole id.

Migration m0001 creates the group role and records its name in
`plugin_group_role`, so a database cloned from a template keeps the group its
grants name. Each plugin role is `LOGIN INHERIT` with no other attribute, a
member of the group, and the owner of its schema. The group holds `USAGE` on
`public` and the default `SELECT` grants, and nothing else. The role's
`search_path` is its schema followed by the app's own path, so
`CREATE TABLE job (...)` lands in `plugin_<id>` and `FROM submission` finds
the host table. `Db::ensure_schema` in the server SDK therefore creates a
plugin's objects in its own schema with no change to the plugin.

No default privilege is set, so a table the host adds later is unreadable to
plugins until it is listed;
`plugin_db::grants::every_host_relation_is_classified` fails on any host
relation that is neither listed nor named hidden.

## Provisioning

`schema::provision` runs the first time a plugin runs SQL on a server, on the
host function pool, as the app role, in one transaction:

1. insert a random password into `plugin_login_secret` for the role, keeping
   an existing one, and lock that row;
2. create the role if it is missing, then set `LOGIN INHERIT NOCREATEROLE`
   and the stored password (a role may name only attributes it holds, so the
   app role names no others; `CREATE ROLE` leaves them off);
3. grant the group to the role, and the role to the app role so the app may
   hand it the schema;
4. `CREATE SCHEMA IF NOT EXISTS ... AUTHORIZATION` the role;
5. set the role's `search_path`;
6. revoke every content column and grant the declared ones.

The row lock serializes replicas provisioning the same plugin, and no plugin
can take it: `plugin_login_secret` grants nothing to plugins. Every replica
reads back the first writer's password, so all of them connect with the same
one. The DDL is part of the schema stamp (`SCHEMA_SOURCES` in
`packages/server/build.rs`).

The app role needs `LOGIN` and `CREATEROLE`, owns the database, and is not a
superuser; the operator configuration reference states this once, and every
installer creates it so. Without `CREATEROLE`, m0001 skips the group role with
a warning. Provisioning then fails: the plugin's calls that run SQL fail as
unavailable, which callers answer with 503, the failure is logged once at
error level, and the next attempt waits 30 seconds (`PROVISION_BACKOFF`).

A plugin removed from the fleet has its role set `NOLOGIN` and its sessions
ended (`schema::retire`). Its schema and data stay: the plugin id is the
plugin's identity, so a later install of the same id is the same plugin, and
provisioning lets its role log in again. Every replica drops its pool for a
plugin it unloads, so the next load provisions again.

## Pools

`PluginSqlPools` (`packages/server/src/plugin_db/mod.rs`) builds one pool per
plugin the first time it runs SQL. Its connections authenticate as the
plugin's role, so a call pays no `SET ROLE` or `SET search_path`, and
`RESET ROLE` lands on the plugin's own role. `SET ROLE` to the group only
drops the plugin's own schema.

All plugin pools share one budget, `PoolSizes::plugin` (one per two plugin CPU
tokens, 2 to 8; the host function pool, which carries the judging writes, gets
one per token, 4 to 16):

- a lease takes one of `budget` permits; at most `budget` plugin calls run SQL
  at once;
- a call that already holds a permit, through an open transaction or a lease
  of its own or of the call it is nested in, reuses it for every further
  statement and transaction, so a call never waits on a permit it holds
  (`plugin_core::executor::CallLocals` carries it across the call tree);
- a plugin with no idle connection, while the pools already hold `budget`
  connections, first closes an idle connection of another plugin;
- a connection released while the pools hold more than `budget` is closed
  rather than kept;
- a connection returning to its pool releases its session advisory locks,
  stops its `LISTEN`s and resets its session settings (`SESSION_RESET`), so a
  lock on the host's boot key or a role change ends with the call.

Host functions other than `host.db.*` run on the host function pool
(`connect_host_fn_pool` in `packages/server/src/database.rs`), as the app
role, with statements the host builds. No plugin-authored SQL runs there.

## The SQL guard

`reject_session_escape` in `host_funcs/sql.rs` refuses role changes,
`set_config`, `ALTER ROLE` and `ALTER USER`, `DISCARD`, `DO`, routine
definitions and session-level changes to the host-enforced settings. The
engine confines role changes; the guard keeps a plugin from changing its own
role's password or defaults, and from leaving its connection unbounded by
`statement_timeout`.

## What the boundary does not cover

- Advisory locks, `LISTEN` and `NOTIFY` are open to every role. Within a call
  a plugin can hold a lock key the host uses or send a claim wake-up on
  `broccoli_claim`; that delays work, it does not change host state, and a
  session lock ends when the connection returns to its pool.
- Large objects: any role can create them, so a plugin can store data outside
  its schema with `lo_from_bytea`. Revoking the `lo_*` functions needs the
  superuser that owns them. A plugin cannot read another role's large objects.
- System catalogs are readable by every role, so a plugin can list role and
  table names, not their contents.

## Tests

`packages/server/tests/integration/plugin_db.rs` runs each test on an empty
database owned by a non-superuser `LOGIN CREATEROLE` app role, on
test-support's shared container, through the real migration and the real
pools, so each rule is enforced by Postgres. The integration and e2e
harnesses log in as that role too.

| Test | Rule |
| --- | --- |
| `a_plugin_has_full_ddl_and_dml_in_its_own_schema` | Own schema: create, alter, index, truncate, drop, foreign keys |
| `a_plugin_cannot_touch_another_plugins_schema` | No read, write or DDL in another plugin's schema, no `SET ROLE` to its role |
| `a_role_escape_lands_on_the_plugins_own_role` | `RESET ROLE` stays on the plugin role; `SET ROLE postgres` is refused; a step down ends with the call |
| `a_plugin_cannot_read_the_plugin_logins` | `plugin_login_secret` is unreadable |
| `grants::every_host_relation_is_classified` | Every host relation is listed readable or hidden |
| `grants::a_plugin_cannot_write_any_host_relation` | `INSERT`, `UPDATE`, `DELETE`, `TRUNCATE`, `ALTER`, `DROP` and `CREATE TRIGGER` refused on every host relation, and no write, `REFERENCES` or `TRIGGER` privilege held on any |
| `grants::a_plugin_cannot_use_host_sequences_or_create_in_public` | No sequence use, no `CREATE` in `public`, no new schema |
| `grants::a_plugin_reads_contest_data_but_no_secret` | Readable columns read; content columns, secret columns and hidden relations refused |
| `grants::a_declared_scope_grants_exactly_its_columns` | A declared scope grants its column to that plugin alone; a dropped scope is revoked |
| `concurrent_first_uses_on_several_replicas_converge` | Concurrent provisioning on four replicas, one password per role |
| `the_plugin_pools_keep_to_one_budget` | Leases past the budget wait |
| `a_call_reuses_its_permit_at_a_full_budget` | A call's statements and nested calls reuse its permit; another call waits |
| `failed_provisioning_backs_off` | A failed provisioning is not retried until the back-off passes |
| `a_returned_connection_drops_its_session_state` | Session locks, listens and settings end when a connection returns |
| `a_removed_plugin_cannot_log_in_until_it_is_installed_again` | Retirement sets `NOLOGIN`; a reinstall logs in and keeps the data |

`plugin_core::manifest` tests reject unknown scopes and scopes without the
`sql` permission at parse time.
