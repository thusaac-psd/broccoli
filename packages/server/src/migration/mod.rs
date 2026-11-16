//! Versioned schema migrations for what the entity `sync()` in
//! [`crate::database`] cannot express. The entity sync creates the tables
//! first; the migrator then runs each step once, recorded in
//! `seaql_migrations` (see [`crate::database::BootSession::migrate`]).

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use sea_orm_migration::prelude::*;

mod m0001_supplementary_schema;

/// Fail before any schema change when the database records a migration this
/// build does not define: it was created by a different build, and this one
/// only ever starts from an empty database.
pub(crate) async fn refuse_unknown_migrations(db: &DatabaseConnection) -> Result<(), DbErr> {
    let known: Vec<String> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let backend = db.get_database_backend();
    let recorded = db
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT to_regclass('seaql_migrations') IS NOT NULL AS present",
        ))
        .await?
        .map(|row| row.try_get::<bool>("", "present"))
        .transpose()?
        .unwrap_or(false);
    if !recorded {
        return Ok(());
    }
    let unknown: Vec<String> = db
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            "SELECT version FROM seaql_migrations WHERE NOT (version = ANY($1)) ORDER BY version",
            [known.into()],
        ))
        .await?
        .into_iter()
        .map(|row| row.try_get::<String>("", "version"))
        .collect::<Result<_, _>>()?;
    if unknown.is_empty() {
        return Ok(());
    }
    Err(DbErr::Custom(format!(
        "this database was created by a different build (it records migrations {}, which this \
         build does not define) and cannot be upgraded; drop the volume or point \
         database.url at an empty database",
        unknown.join(", ")
    )))
}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m0001_supplementary_schema::Migration)]
    }
}
