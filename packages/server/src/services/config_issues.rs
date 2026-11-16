//! A problem's `config_issues` flag (`problem::Model::config_issues`): whether
//! any of its stored configs, at the problem scope or in a contest that uses
//! it, is refused by its plugin as things stand ([`problem_config_issues`]).
//! Judging copies the flag onto every judgement it claims, so it must follow
//! whatever can change the answer:
//!
//! - a test-data change sets it in its own commit
//!   ([`crate::services::test_data::TestDataEdit`]);
//! - every write or delete of a stored config, an admin's, a plugin's or the
//!   host's own as a problem or contest goes, returns the
//!   `services::plugin_config::ConfigChange` that rechecks the problems it
//!   reaches once it commits:
//!   a problem or contest-problem config is checked before it is stored, so
//!   its change can only clear the flag; a contest or plugin-scope config is
//!   what other configs are checked against, so its change moves the flag
//!   either way;
//! - a change to a plugin (enabled, disabled, reloaded, upgraded, its upload
//!   removed) changes which configs are checked and how, so it rechecks the
//!   problems with a config of that plugin, both ways
//!   ([`recheck_problems_of_plugins`]).
//!
//! The rechecks run after the change committed, on the action that made it,
//! never on the judging path. A recheck that fails is logged and leaves the
//! flag as it was.

use std::collections::BTreeSet;

use common::db::{DatabaseConnection, DatabaseTransaction};
use plugin_core::traits::PluginManager;
use sea_orm::sea_query::{Expr, LockType};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, TransactionTrait};

use crate::entity::problem;
use crate::error::AppError;
use crate::services::plugin_config::{Reads, problem_config_issues, problems_with_configs_of};

/// Which way a recheck may move the flag; `Both` covers `ClearOnly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Recheck {
    /// Only clear a set flag: the change cannot have added an issue.
    ClearOnly,
    /// Set or clear it to match the configs.
    Both,
}

/// Records whether problem `problem_id` has config issues, in `txn`.
pub(crate) async fn set_config_issues(
    txn: &DatabaseTransaction,
    problem_id: i32,
    has_issues: bool,
) -> Result<(), AppError> {
    problem::Entity::update_many()
        .col_expr(problem::Column::ConfigIssues, Expr::value(has_issues))
        .filter(problem::Column::Id.eq(problem_id))
        .exec(txn)
        .await?;
    Ok(())
}

/// Rechecks the flag of every problem with a stored config of one of
/// `plugin_ids`, after a change to those plugins.
pub(crate) async fn recheck_problems_of_plugins(
    db: &DatabaseConnection,
    plugins: &dyn PluginManager,
    plugin_ids: &[&str],
) {
    match problems_with_configs_of(db, Some(plugin_ids)).await {
        Ok(problems) => {
            recheck_problems(db, plugins, problems, Recheck::Both, Reads::Committed).await
        }
        Err(error) => tracing::warn!(
            ?plugin_ids,
            ?error,
            "could not find the problems whose config issues a plugin change affects"
        ),
    }
}

/// Rechecks the flag of each of `problems`, moving it as `recheck` allows,
/// the plugins reading as `reads` says.
pub(crate) async fn recheck_problems(
    db: &DatabaseConnection,
    plugins: &dyn PluginManager,
    problems: BTreeSet<i32>,
    recheck: Recheck,
    reads: Reads<'_>,
) {
    for problem_id in problems {
        if let Err(error) = try_recheck(db, plugins, problem_id, recheck, reads).await {
            tracing::warn!(
                problem_id,
                ?error,
                "could not recheck the problem's config issues"
            );
        }
    }
}

/// Rechecks one problem under its row lock, the lock a test-data change
/// holds, so a recheck never races the test data it checks against.
async fn try_recheck(
    db: &DatabaseConnection,
    plugins: &dyn PluginManager,
    problem_id: i32,
    recheck: Recheck,
    reads: Reads<'_>,
) -> Result<(), AppError> {
    let txn = db.begin().await?;
    let Some(row) = problem::Entity::find_by_id(problem_id)
        .lock(LockType::Update)
        .one(&txn)
        .await?
    else {
        return Ok(());
    };
    if recheck == Recheck::ClearOnly && !row.config_issues {
        return Ok(());
    }
    let has_issues = !problem_config_issues(&txn, plugins, problem_id, true, reads)
        .await?
        .is_empty();
    let moves = match recheck {
        Recheck::ClearOnly => !has_issues,
        Recheck::Both => has_issues != row.config_issues,
    };
    if moves {
        set_config_issues(&txn, problem_id, has_issues).await?;
        txn.commit().await?;
    }
    Ok(())
}
