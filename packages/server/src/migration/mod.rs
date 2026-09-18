//! Versioned, tracked schema migrations for the supplementary DDL that the
//! entity `sync()` in [`crate::database`] does not cover: extensions, legacy
//! constraint drops, column evolutions on existing tables, one data backfill,
//! and the `broccoli_plugin` least-privilege role + read-narrowing grants/views.
//!
//! These previously ran as an ad-hoc, per-boot block of `execute_unprepared`
//! (several with a silent `let _ =`). Running them through `sea_orm_migration`'s
//! [`MigratorTrait`] records each in the `seaql_migrations` table, so they apply
//! exactly once and their failures surface instead of being swallowed. Every
//! step is idempotent (`IF [NOT] EXISTS` / exception-guarded `DO` blocks), so an
//! EXISTING deployment whose schema is already fully applied simply records the
//! migrations as no-ops on first run.
//!
//! `sync()` still creates/updates the core tables from the entities; the
//! migrator runs AFTER it (see [`crate::database::init_db_with_max_connections`]).

use sea_orm_migration::prelude::*;

mod m0001_supplementary_ddl;
mod m0002_user_credentials_changed_at;
mod m0003_remove_plugin_read_denylist;
mod m0004_grant_plugin_writes;
mod m0005_leased_at;
mod m0006_plugin_timer_index;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0001_supplementary_ddl::Migration),
            Box::new(m0002_user_credentials_changed_at::Migration),
            Box::new(m0003_remove_plugin_read_denylist::Migration),
            Box::new(m0004_grant_plugin_writes::Migration),
            Box::new(m0005_leased_at::Migration),
            Box::new(m0006_plugin_timer_index::Migration),
        ]
    }
}
