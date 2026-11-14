---
title: Schema migrations
sidebar_label: Schema migrations
sidebar_position: 9
---

# Schema migrations

The server owns the database schema as an ordered list of forward migrations
in `packages/server/src/migration`. Workers and the storage layer read and
write their tables (`task_cache`, `blob_data`) but never create or change them.

## What boot does

Every server boot opens one session, takes the boot lock (the advisory lock
`BOOT_LOCK_KEY` in `packages/server/src/database.rs`) and calls
`BootSession::migrate`:

1. `check_applied_versions` reads `seaql_migrations`. An empty database passes.
   A database with tables but no `seaql_migrations` is refused, and so is one
   that records a migration this build does not define; the error names that
   migration and this build's newest one. Nothing is changed before either
   refusal.
2. `Migrator::up` applies the pending migrations, oldest first, each in its own
   transaction, recording each in `seaql_migrations`.

Servers booting together queue on the lock. The first applies what is
pending; the others then find nothing pending. Seeding runs under the same
lock afterwards.

## The baseline

`m0001_baseline` creates the whole schema. `m0001_baseline.sql` holds every
table, index, constraint, sequence, trigger function and trigger. The Rust
side adds what depends on the cluster: the optional `pg_stat_statements`
extension and the database's plugin group role with its grants (see
[plugin database access](plugin-database.md)).

## Changing the schema

A schema change is a new migration, never an edit to a shipped one:

1. Add `packages/server/src/migration/mNNNN_<what>.rs` implementing
   `MigrationTrait` with the forward DDL, and append it to
   `Migrator::migrations`.
2. Change the entities (and any code-owned trigger definition) to match.
3. Run the schema test.

`down` is not implemented: a rollback restores the backup taken before the
upgrade.

## The schema test

`migration::tests::the_migrations_create_the_schema_the_code_expects` migrates
an empty database and compares it with what the code expects:

- It snapshots the catalog (columns with type, nullability and default,
  constraints, indexes, triggers, functions, sequences), then runs entity
  schema sync, creates the `blob_data` and `task_cache` entity tables, and
  runs the code-owned trigger definitions (`judging::notify::expected` and
  `plugin_config_cache::expected`). Any change to the catalog fails the test.
- Sync never alters an existing column, so it also builds a second database
  from the entities alone and compares each entity column's type and
  nullability.

`the_claim_indexes_match_the_predicates_the_queue_scans_use` checks that each
partial claim index has the predicate the queue scans write inline, so the
planner keeps using it.

```bash
cargo test -p server --lib migration::
```

## Rolling upgrades

An older server keeps working against a database a newer build migrated while
each new migration is additive: a new table, a new nullable or defaulted
column, a new index. Anything else (a rename, a drop, a type change, a new
`NOT NULL` without a default, a tighter constraint) follows expand/contract
over two releases. The first adds the new shape and keeps writing the old;
the second, once no server runs the old release, drops the old shape. An older
server that restarts after the database moved ahead refuses to boot, so an
operator finishes a roll once it starts. The operator steps are in
`release/docs/upgrade.md`.
