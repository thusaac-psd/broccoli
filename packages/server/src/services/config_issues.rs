//! A problem's `config_issues` flag (`problem::Model::config_issues`): whether
//! any of its stored configs, at the problem scope or in a contest that uses
//! it, is refused by its plugin as things stand ([`problem_config_issues`]).
//! Judging copies the flag onto every judgement it claims, so it must follow
//! whatever can change the answer:
//!
//! - a test-data change sets it in its own commit
//!   ([`crate::services::test_data::TestDataEdit`]);
//! - a problem or contest-problem config write can only resolve issues, since
//!   the write itself is checked, so it clears a flag that no longer holds
//!   ([`clear_resolved_config_issues`]);
//! - a change to a plugin (enabled, disabled, reloaded, upgraded, its upload
//!   removed) changes which configs are checked and how, and a plugin- or
//!   contest-scope config write changes what a check may read: each rechecks
//!   the problems it reaches, both ways ([`recheck_problems_of_plugins`],
//!   [`recheck_problems_of_contest`]).
//!
//! The rechecks run after the change committed, on the admin action that made
//! it, never on the judging path. A recheck that fails is logged and leaves
//! the flag as it was.

use std::collections::BTreeSet;

use common::db::{DatabaseConnection, DatabaseTransaction};
use plugin_core::traits::PluginManager;
use sea_orm::sea_query::{Expr, LockType};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, TransactionTrait};

use crate::entity::{contest_problem, problem};
use crate::error::AppError;
use crate::services::plugin_config::{Reads, problem_config_issues, problems_with_configs_of};

/// Which way a recheck may move the flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recheck {
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

/// Clears problem `problem_id`'s flag once a config write leaves none of its
/// stored configs with issues. A config write is checked before it is stored,
/// so it can resolve issues but never add one; when the flag is clear there
/// is nothing to check.
pub(crate) async fn clear_resolved_config_issues(
    db: &DatabaseConnection,
    plugins: &dyn PluginManager,
    problem_id: i32,
) {
    recheck_logged(db, plugins, problem_id, Recheck::ClearOnly).await;
}

/// Rechecks the flag of every problem with a stored config of one of
/// `plugin_ids`, after a change to those plugins.
pub(crate) async fn recheck_problems_of_plugins(
    db: &DatabaseConnection,
    plugins: &dyn PluginManager,
    plugin_ids: &[&str],
) {
    match problems_with_configs_of(db, plugin_ids).await {
        Ok(problems) => recheck_all(db, plugins, problems).await,
        Err(error) => tracing::warn!(
            ?plugin_ids,
            ?error,
            "could not find the problems whose config issues a plugin change affects"
        ),
    }
}

/// Rechecks the flag of every problem in contest `contest_id`, after a write
/// to the contest's own configs.
pub(crate) async fn recheck_problems_of_contest(
    db: &DatabaseConnection,
    plugins: &dyn PluginManager,
    contest_id: i32,
) {
    let problems = contest_problem::Entity::find()
        .select_only()
        .column(contest_problem::Column::ProblemId)
        .filter(contest_problem::Column::ContestId.eq(contest_id))
        .into_tuple::<i32>()
        .all(db)
        .await;
    match problems {
        Ok(problems) => recheck_all(db, plugins, problems.into_iter().collect()).await,
        Err(error) => tracing::warn!(
            contest_id,
            ?error,
            "could not find the problems whose config issues a contest config write affects"
        ),
    }
}

async fn recheck_all(
    db: &DatabaseConnection,
    plugins: &dyn PluginManager,
    problems: BTreeSet<i32>,
) {
    for problem_id in problems {
        recheck_logged(db, plugins, problem_id, Recheck::Both).await;
    }
}

async fn recheck_logged(
    db: &DatabaseConnection,
    plugins: &dyn PluginManager,
    problem_id: i32,
    recheck: Recheck,
) {
    if let Err(error) = try_recheck(db, plugins, problem_id, recheck).await {
        tracing::warn!(
            problem_id,
            ?error,
            "could not recheck the problem's config issues"
        );
    }
}

/// Rechecks one problem under its row lock, the lock a test-data change
/// holds, so a recheck never races the test data it checks against.
async fn try_recheck(
    db: &DatabaseConnection,
    plugins: &dyn PluginManager,
    problem_id: i32,
    recheck: Recheck,
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
    let has_issues = !problem_config_issues(&txn, plugins, problem_id, true, Reads::Committed)
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
