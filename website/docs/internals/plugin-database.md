---
title: Plugin database access
sidebar_label: Plugin database access
sidebar_position: 6
---

# Plugin database access

A plugin with the `sql` permission runs SQL through `host.db.*`
(`packages/server/src/host_funcs/sql.rs`). Postgres decides what that SQL may
do: every plugin logs in as a role of its own, owns one schema, and can read a
curated set of host tables. Host state changes only through host functions,
which carry the guards the host relies on (the judging epoch fences and the
finalized guard, DLQ ownership, the `plugin_config` change counter, accounts
and tokens).

## What a plugin may do

| Object | Privilege |
| --- | --- |
| Its schema `plugin_<id>` | Owner: full DDL and DML, `TRUNCATE`, foreign keys between its own tables |
| Contest data in `public` (`PLUGIN_READABLE` in m0001) | `SELECT` |
| `"user"` | `SELECT` on `id`, `username`, `display_name`, `created_at`, `deleted_at` |
| Every other host relation | Nothing |
| Host sequences, schema `public` | Nothing; `CREATE` on `public` is revoked from `PUBLIC` too |
| Another plugin's schema | Nothing |

The readable relations are contest data with no secret in them: contests,
problems, test cases, submissions, judgements, results, code runs and
clarifications, plus the `plugin_user_public` view. Unreadable: password
hashes and the credential timestamp, refresh tokens, device codes, idempotency
keys and bodies, the plugin login secrets, the blob store, roles and
permissions, the plugin registry, every plugin's host-held state
(`plugin_config`, `plugin_storage`, `plugin_timer`, `plugin_replica`,
`submission_feedback`), the DLQ, and the schema stamp and migration record.

Writes to host state go through host functions: `host.submission.*` and
`host.code_run.*` for judgements and results, `host.storage.*`,
`config:read`/`config:write`, `host.timer.*` and the submission feedback
functions. None of the shipped plugins writes a host table directly.

## Roles and schemas

`packages/server/src/plugin_db/schema.rs` derives every name from the plugin
id, so the host holds no per-plugin knowledge:

- the schema is `plugin_<id>`;
- the login role is `broccoli_<db>_<id>`, where `<db>` is a digest of the
  database name, because roles are shared by every database of a cluster;
- an id too long for Postgres' 63-byte identifiers keeps its head and ends in
  a digest of the whole id.

Each plugin role is `LOGIN INHERIT` with no other attribute, a member of the
NOLOGIN group role `broccoli_plugin`, and the owner of its schema. The group
role holds `USAGE` on `public` and the `SELECT` grants, and nothing else. The
role's `search_path` is its schema followed by the app's own path, so
`CREATE TABLE job (...)` lands in `plugin_<id>` and `FROM submission` finds the
host table. `Db::ensure_schema` in the server SDK therefore creates a plugin's
objects in its own schema with no change to the plugin.

The grants on host relations are applied by migration m0001
(`PLUGIN_READABLE`, `PLUGIN_READABLE_COLUMNS`, `plugin_role`). No default
privilege is set, so a table the host adds later is unreadable to plugins until
it is listed; `plugin_db::tests::grants::every_host_relation_is_classified`
fails on any host relation that is neither listed nor named hidden.

## Provisioning

`schema::provision` runs the first time a plugin runs SQL on a server, on the
host function pool, as the app role, in one transaction:

1. insert a random password into `plugin_login_secret` for the role, keeping
   an existing one, and lock that row;
2. create the role if it is missing, then set its attributes and the stored
   password;
3. grant `broccoli_plugin` to the role, and the role to the app role so the
   app may hand it the schema;
4. `CREATE SCHEMA IF NOT EXISTS ... AUTHORIZATION` the role;
5. set the role's `search_path`.

The row lock serializes replicas provisioning the same plugin, and no plugin
can take it: `plugin_login_secret` grants nothing to plugins. Every replica
reads back the first writer's password, so all of them connect with the same
one. The DDL is part of the schema stamp (`SCHEMA_SOURCES` in
`packages/server/build.rs`).

Creating roles needs CREATEROLE. Without it m0001 skips the group role with a
warning and provisioning fails, so `host.db.*` returns an error and no plugin
SQL runs with more than its own rights.

## Pools

`PluginSqlPools` (`packages/server/src/plugin_db/mod.rs`) builds one pool per
plugin the first time it runs SQL. Its connections authenticate as the
plugin's role, so a call pays no `SET ROLE` or `SET search_path`, and
`RESET ROLE` lands on the plugin's own role. `SET ROLE broccoli_plugin` only
drops the plugin's own schema.

All plugin pools share one connection budget, `PoolSizes::plugin`:

- a lease takes one of `budget` permits for a statement, or for a
  transaction until it ends, so at most `budget` connections are in use;
- a plugin with no idle connection, while the pools already hold `budget`,
  first closes an idle connection of another plugin;
- a connection released while the pools hold more than `budget` is closed
  rather than kept, which covers first uses racing past the previous check and
  connections still on their way back to their pool.

Host functions other than `host.db.*` run on the host function pool
(`connect_host_fn_pool` in `packages/server/src/database.rs`), as the app
role, with statements the host builds. No plugin-authored SQL runs there, and
no plugin is granted more than the rights above: there is no privileged plugin
tier to declare in a manifest.

## The SQL guard

`reject_session_escape` in `host_funcs/sql.rs` still refuses role changes,
`set_config`, `DISCARD`, `DO`, routine definitions and session-level changes
to the host-enforced settings. The engine already confines role changes; the
guard keeps a plugin from leaving its pooled connection unbounded by
`statement_timeout` for its next call.

## What the boundary does not cover

- Advisory locks, `LISTEN` and `NOTIFY` are open to every role. A plugin can
  hold a lock key the host uses or send a claim wake-up on `broccoli_claim`;
  that delays work, it does not change host state.
- System catalogs are readable by every role, so a plugin can list role and
  table names, not their contents.

## Tests

`packages/server/src/plugin_db/tests.rs` boots a fresh database per test and
drives the real pools, so each rule is enforced by Postgres:

| Test | Rule |
| --- | --- |
| `a_plugin_has_full_ddl_and_dml_in_its_own_schema` | Own schema: create, alter, index, truncate, drop, foreign keys |
| `a_plugin_cannot_touch_another_plugins_schema` | No read, write or DDL in another plugin's schema, no `SET ROLE` to its role |
| `a_role_escape_lands_on_the_plugins_own_role` | `RESET ROLE` stays on the plugin role; `SET ROLE postgres` is refused |
| `a_plugin_cannot_read_the_plugin_logins` | `plugin_login_secret` is unreadable |
| `grants::every_host_relation_is_classified` | Every host relation is listed readable or hidden |
| `grants::a_plugin_cannot_write_any_host_relation` | `INSERT`, `UPDATE`, `DELETE`, `TRUNCATE`, `ALTER`, `DROP` and `CREATE TRIGGER` refused on every host relation, and no write, `REFERENCES` or `TRIGGER` privilege held on any |
| `grants::a_plugin_cannot_use_host_sequences_or_create_in_public` | No sequence use, no `CREATE` in `public`, no new schema |
| `grants::a_plugin_reads_contest_data_but_no_secret` | Readable relations read, secret columns and hidden relations refused |
| `concurrent_first_uses_on_several_replicas_converge` | Concurrent provisioning on four replicas, one password per role |
| `the_plugin_pools_keep_to_one_budget` | Pools stay within the budget; leases past it wait |
