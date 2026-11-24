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
//! - The test `a_database_the_released_build_ran_upgrades_to_this_build`
//!   migrates a database the released build ran, with rows in that build's
//!   shapes, through every pending migration, so a migration that assumes an
//!   empty table fails in CI instead of on an operator's upgrade. Its
//!   `RELEASED` names the last migration of the release installations run.
//!
//! The server only migrates forward: boot runs `up` for the pending
//! migrations, and nothing in the server or its tools calls `down`. Each
//! migration's `down` reverses its `up` for a developer stepping a scratch
//! database back; an installation rolls back by restoring the dump taken
//! before the upgrade (`release/docs/upgrade.md`).
//!
//! A database is refused when its history is not a prefix of this build's
//! list: one that runs past this build's latest migration was migrated by a
//! newer build, and one that does not start from this build's baseline (or
//! records versions the list has no place for) predates the baseline or
//! comes from an unrelated build. Neither has a migration path from here.

use common::db::DatabaseConnection;
use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

mod m0001_baseline;
mod m0002_checker_format_no_default;
mod m0003_username_case_insensitive;
mod m0004_platform_setting;
mod m0005_no_plugin_column_defaults;
mod m0006_auth_session;
mod m0007_user_password_optional;
mod m0008_user_password_not_blank;
mod m0009_config_issues;
mod m0010_error_stands;
mod m0011_plugin_diagnostics_hidden;
mod m0012_refresh_token_session_required;
mod m0013_device_approval_session;
mod m0014_device_code_client_addr;
mod m0015_config_recheck;
mod m0016_plugin_host_locale;
mod m0017_plugin_governs_shape;
mod m0018_judging_inputs_version;
mod m0019_replica_provides;
mod m0020_hook_event;

pub use m0001_baseline::{PLUGIN_READABLE, PLUGIN_READABLE_COLUMNS};

/// The migrations this build defines, oldest first.
pub(crate) fn known_migrations<M: MigratorTrait>() -> Vec<String> {
    M::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect()
}

/// Fail before any schema change unless the database is empty or was migrated
/// by this build or an older one of the same baseline (see
/// [`check_history`]): applying this build's migrations to any other schema
/// could corrupt it.
pub(crate) async fn check_applied_versions<M: MigratorTrait>(
    db: &DatabaseConnection,
) -> Result<(), DbErr> {
    let known = known_migrations::<M>();
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
    check_history(&known, &applied).map_err(|mismatch| DbErr::Custom(mismatch.to_string()))
}

/// How a database's migration history disagrees with this build's.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HistoryMismatch {
    /// The history starts from this build's baseline and adds versions after
    /// its latest: a newer build migrated the database.
    Newer {
        unknown: Vec<String>,
        latest: String,
    },
    /// The history does not start from this build's baseline, or holds
    /// versions this build's list has no place for: the database predates
    /// the baseline or is not one any build of this line created.
    Foreign {
        unknown: Vec<String>,
        baseline: String,
    },
}

impl std::fmt::Display for HistoryMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HistoryMismatch::Newer { unknown, latest } => write!(
                f,
                "the database schema is at {}, which this build does not know (it was migrated \
                 by a newer build); this build's schema is {latest}. Run a build at least as new \
                 as the one that migrated the database, or restore the backup taken before that \
                 upgrade",
                unknown.join(", ")
            ),
            HistoryMismatch::Foreign { unknown, baseline } => {
                let records = if unknown.is_empty() {
                    String::new()
                } else {
                    format!(" (it records {})", unknown.join(", "))
                };
                write!(
                    f,
                    "the database's migration history does not start from this build's baseline \
                     {baseline}{records}: it predates the baseline or was created by an \
                     unrelated build, and no migration path leads from it. Recreate the database \
                     or restore a backup taken with a compatible build"
                )
            }
        }
    }
}

/// Checks an applied migration history against `known`, this build's list,
/// oldest first. A history is accepted when it is empty, or starts from the
/// baseline and records only versions `known` defines; one that adds versions
/// after the latest was written by a newer build; anything else is foreign. Versions are compared as recorded:
/// `mNNNN_` names sort in the order they were appended.
pub(crate) fn check_history(known: &[String], applied: &[String]) -> Result<(), HistoryMismatch> {
    let unknown: Vec<String> = applied
        .iter()
        .filter(|version| !known.contains(version))
        .cloned()
        .collect();
    let baseline = known.first().cloned().unwrap_or_default();
    let latest = known.last().cloned().unwrap_or_default();
    let has_baseline = applied.is_empty() || applied.contains(&baseline);
    if unknown.is_empty() && has_baseline {
        return Ok(());
    }
    if has_baseline && unknown.iter().all(|version| *version > latest) {
        return Err(HistoryMismatch::Newer { unknown, latest });
    }
    Err(HistoryMismatch::Foreign { unknown, baseline })
}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0001_baseline::Migration),
            Box::new(m0002_checker_format_no_default::Migration),
            Box::new(m0003_username_case_insensitive::Migration),
            Box::new(m0004_platform_setting::Migration),
            Box::new(m0005_no_plugin_column_defaults::Migration),
            Box::new(m0006_auth_session::Migration),
            Box::new(m0007_user_password_optional::Migration),
            Box::new(m0008_user_password_not_blank::Migration),
            Box::new(m0009_config_issues::Migration),
            Box::new(m0010_error_stands::Migration),
            Box::new(m0011_plugin_diagnostics_hidden::Migration),
            Box::new(m0012_refresh_token_session_required::Migration),
            Box::new(m0013_device_approval_session::Migration),
            Box::new(m0014_device_code_client_addr::Migration),
            Box::new(m0015_config_recheck::Migration),
            Box::new(m0016_plugin_host_locale::Migration),
            Box::new(m0017_plugin_governs_shape::Migration),
            Box::new(m0018_judging_inputs_version::Migration),
            Box::new(m0019_replica_provides::Migration),
            Box::new(m0020_hook_event::Migration),
        ]
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod history_tests {
    use super::{HistoryMismatch, check_history};

    fn list(versions: &[&str]) -> Vec<String> {
        versions.iter().map(|v| v.to_string()).collect()
    }

    fn known() -> Vec<String> {
        list(&["m0001_baseline", "m0002_b", "m0003_c"])
    }

    #[test]
    fn an_empty_or_older_history_is_accepted() {
        assert_eq!(check_history(&known(), &[]), Ok(()));
        assert_eq!(check_history(&known(), &list(&["m0001_baseline"])), Ok(()));
        assert_eq!(check_history(&known(), &known()), Ok(()));
    }

    #[test]
    fn versions_after_the_latest_mean_a_newer_build() {
        let applied = list(&["m0001_baseline", "m0002_b", "m0003_c", "m0004_d"]);
        assert_eq!(
            check_history(&known(), &applied),
            Err(HistoryMismatch::Newer {
                unknown: list(&["m0004_d"]),
                latest: "m0003_c".into(),
            })
        );
    }

    #[test]
    fn a_history_without_this_builds_baseline_is_foreign_not_newer() {
        // A database from before the baseline was squashed: its versions
        // sort after this build's latest only by name.
        let applied = list(&["m20250101_000001_create_users", "m20250301_000002_add_x"]);
        let err = check_history(&known(), &applied).unwrap_err();
        assert!(matches!(err, HistoryMismatch::Foreign { .. }), "{err:?}");
        let message = err.to_string();
        assert!(message.contains("baseline m0001_baseline"), "{message}");
        assert!(!message.contains("newer build"), "{message}");
    }

    #[test]
    fn unknown_versions_inside_the_known_range_are_foreign() {
        let applied = list(&["m0001_baseline", "m0002_other", "m0003_c"]);
        assert!(matches!(
            check_history(&known(), &applied),
            Err(HistoryMismatch::Foreign { .. })
        ));
    }
}
