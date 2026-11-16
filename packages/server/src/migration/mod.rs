//! Versioned schema migrations for what the entity `sync()` in
//! [`crate::database`] cannot express. The entity sync creates the tables
//! first; the migrator then runs each step once, recorded in
//! `seaql_migrations` (see [`crate::database::BootSession::migrate`]).

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use sea_orm_migration::prelude::*;

mod m0001_supplementary_schema;

/// The schema this build creates, as a stamp over its schema sources (see
/// `build.rs`). m0001 records it; boot refuses a database recorded with any
/// other stamp, since schema sync adds tables and columns but never changes
/// what an existing column is (its default, its type).
pub(crate) const SCHEMA_STAMP: &str = env!("BROCCOLI_SCHEMA_STAMP");

/// The table m0001 records [`SCHEMA_STAMP`] in.
pub(crate) const STAMP_TABLE: &str = "broccoli_schema_stamp";

fn needs_fresh_database(reason: String) -> DbErr {
    DbErr::Custom(format!(
        "this database was created by a different build ({reason}) and cannot be upgraded; \
         drop the volume or point database.url at an empty database"
    ))
}

/// Fail before any schema change when the database was created by another
/// build: it records a migration this build does not define, or a schema
/// stamp other than [`SCHEMA_STAMP`]. This build only ever starts from an
/// empty database or one it created.
pub(crate) async fn refuse_foreign_schema(db: &DatabaseConnection) -> Result<(), DbErr> {
    let known: Vec<String> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let backend = db.get_database_backend();
    let row = db
        .query_one_raw(Statement::from_string(
            backend,
            format!(
                "SELECT to_regclass('seaql_migrations') IS NOT NULL AS migrated, \
                        to_regclass('{STAMP_TABLE}') IS NOT NULL AS stamped"
            ),
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("catalog query returned no row".into()))?;
    if !row.try_get::<bool>("", "migrated")? {
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
    if !unknown.is_empty() {
        return Err(needs_fresh_database(format!(
            "it records migrations {}, which this build does not define",
            unknown.join(", ")
        )));
    }
    let stamp: Option<String> = if row.try_get::<bool>("", "stamped")? {
        db.query_one_raw(Statement::from_string(
            backend,
            format!("SELECT stamp FROM {STAMP_TABLE}"),
        ))
        .await?
        .map(|r| r.try_get("", "stamp"))
        .transpose()?
    } else {
        None
    };
    match stamp {
        Some(stamp) if stamp == SCHEMA_STAMP => Ok(()),
        Some(stamp) => Err(needs_fresh_database(format!(
            "schema {stamp}, this build creates {SCHEMA_STAMP}"
        ))),
        None => Err(needs_fresh_database("it records no schema stamp".into())),
    }
}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m0001_supplementary_schema::Migration)]
    }
}
