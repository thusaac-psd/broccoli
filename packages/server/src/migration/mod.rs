//! The database schema, as versioned forward migrations. The migrations are
//! the only thing that changes the schema: boot applies the pending ones in
//! order under the boot lock (see [`crate::database::BootSession::migrate`]),
//! and `seaql_migrations` records each applied one.
//!
//! - `m0001_baseline` creates the whole schema.
//! - Every later schema change is a new migration, `mNNNN_<what>`, appended to
//!   [`Migrator::migrations`]. A migration that has shipped is never edited.
//! - The entities and the code-owned trigger definitions describe the schema
//!   the code expects; the test `the_migrations_create_the_schema_the_code_expects`
//!   fails when the migrated database differs from them, so an entity change
//!   without its migration cannot pass CI.
//!
//! A database is refused when it records a migration this build does not
//! define: it was migrated by a newer build, and this one cannot know what
//! that schema means.

use common::db::DatabaseConnection;
use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

mod m0001_baseline;
mod m0002_checker_format_no_default;

pub use m0001_baseline::{PLUGIN_READABLE, PLUGIN_READABLE_COLUMNS};

/// The migrations this build defines, oldest first.
pub(crate) fn known_migrations<M: MigratorTrait>() -> Vec<String> {
    M::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect()
}

/// Fail before any schema change unless the database is empty or was migrated
/// by this build or an older one. Every version it records must be one `M`
/// defines; anything else means a newer build migrated it (or it is not a
/// Broccoli database), and applying this build's migrations could corrupt it.
pub(crate) async fn check_applied_versions<M: MigratorTrait>(
    db: &DatabaseConnection,
) -> Result<(), DbErr> {
    let known = known_migrations::<M>();
    let latest_known = known.last().cloned().unwrap_or_default();
    let backend = db.get_database_backend();
    let row = db
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT to_regclass('seaql_migrations') IS NOT NULL AS migrated, \
                    (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname = current_schema() AND c.relkind IN ('r', 'p', 'v', 'm', 'S') \
                       AND NOT EXISTS (SELECT 1 FROM pg_depend d \
                                       WHERE d.objid = c.oid AND d.deptype = 'e'))::bigint \
                      AS relations",
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("catalog query returned no row".into()))?;
    if !row.try_get::<bool>("", "migrated")? {
        let relations: i64 = row.try_get("", "relations")?;
        return if relations == 0 {
            Ok(())
        } else {
            Err(DbErr::Custom(format!(
                "the database holds {relations} relations but no Broccoli migration record; \
                 point database.url at an empty database or at a Broccoli database"
            )))
        };
    }
    let applied: Vec<String> = db
        .query_all_raw(Statement::from_string(
            backend,
            "SELECT version FROM seaql_migrations ORDER BY version",
        ))
        .await?
        .into_iter()
        .map(|row| row.try_get::<String>("", "version"))
        .collect::<Result<_, _>>()?;
    let unknown: Vec<&str> = applied
        .iter()
        .filter(|version| !known.contains(version))
        .map(String::as_str)
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(DbErr::Custom(format!(
        "the database schema is at {}, which this build does not know (it was migrated by a \
         newer build); this build's schema is {latest_known}. Run a build at least as new as the \
         one that migrated the database, or restore the backup taken before that upgrade",
        unknown.join(", ")
    )))
}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0001_baseline::Migration),
            Box::new(m0002_checker_format_no_default::Migration),
        ]
    }
}

#[cfg(test)]
mod tests;
