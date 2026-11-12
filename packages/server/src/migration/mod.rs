//! Versioned schema migrations for what the entity `sync()` in
//! [`crate::database`] cannot express. The entity sync creates the tables
//! first; the migrator then runs each step once, recorded in
//! `seaql_migrations` (see [`crate::database::BootSession::migrate`]).

use sea_orm_migration::prelude::*;

mod m0001_supplementary_schema;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m0001_supplementary_schema::Migration)]
    }
}
