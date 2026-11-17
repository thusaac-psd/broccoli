//! Changes to a problem's test data, and the stored configs that refer to it.
//!
//! A config can name test data (a subtask lists test case labels), so a
//! change to the test data can leave a config its plugin finds wrong, and
//! judging would score with it. Every change therefore goes through a
//! [`TestDataEdit`], whose only way to commit checks the problem's configs
//! against the data as the change leaves it, before it commits: the same
//! schema and `validate` check a config write passes ([`issues_of`]). A
//! change that gives a config someone saved something wrong it did not have
//! is refused with [`AppError::ConfigWouldBreak`] unless the caller confirms
//! it; what the namespace's defaults find where nothing is stored, such as
//! no test cases yet, never refuses one. Either way the problem's
//! `config_issues` flag records whether any config has issues, which judging
//! copies onto the judgements it claims.

use std::sync::Arc;

use common::db::{DatabaseConnection, DatabaseTransaction};
use sea_orm::sea_query::LockType;
use sea_orm::{QuerySelect, TransactionTrait};

use crate::entity::problem;
use crate::error::AppError;
use crate::services::config_issues::{
    ConfigChecker, ConfigIssue, Subject, issues_of, record_outcome,
};
use crate::services::plugin_config::Reads;
use crate::utils::soft_delete::SoftDeletable;

/// A change to one problem's test data, open in a transaction that holds the
/// problem's row lock, so changes to the same problem's test data run one at
/// a time. Dropped without [`TestDataEdit::commit`], it rolls back.
pub(crate) struct TestDataEdit {
    txn: Arc<DatabaseTransaction>,
    problem: problem::Model,
}

/// Whether a test-data change goes through when it would leave stored configs
/// refused by their plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnConfigBreak {
    /// Refuse it with [`AppError::ConfigWouldBreak`].
    Refuse,
    /// Make it; the problem's config issues list what it broke.
    Allow,
}

impl OnConfigBreak {
    /// What a request's `force` flag asks for.
    pub(crate) fn forced(force: bool) -> Self {
        if force { Self::Allow } else { Self::Refuse }
    }
}

impl TestDataEdit {
    /// Opens a change to problem `problem_id`'s test data; `NOT_FOUND` when
    /// the problem does not exist or is deleted.
    pub(crate) async fn begin(db: &DatabaseConnection, problem_id: i32) -> Result<Self, AppError> {
        let txn = db.begin().await?;
        let problem = problem::Entity::find_active_by_id(problem_id)
            .lock(LockType::Update)
            .one(&txn)
            .await?
            .ok_or_else(|| AppError::NotFound("Problem not found".into()))?;
        Ok(Self {
            txn: Arc::new(txn),
            problem,
        })
    }

    /// The transaction the change is made in.
    pub(crate) fn db(&self) -> &DatabaseTransaction {
        &self.txn
    }

    /// Commits the change, after checking the problem's configs (at the
    /// problem scope and in every contest, stored or the defaults its
    /// plugins check; see [`crate::services::config_issues`]) against the
    /// test data as it leaves them. With [`OnConfigBreak::Refuse`], a change
    /// that gives a stored config an error or an issue it did not have is
    /// refused with what it gives. What the config already had never refuses
    /// a change, nor does what a namespace's defaults find where nothing is
    /// stored, nor what a plugin not loaded here would say: the flag records
    /// all of it ([`crate::services::config_issues::Outcome`]).
    pub(crate) async fn commit(
        self,
        checker: &ConfigChecker<'_>,
        on_break: OnConfigBreak,
    ) -> Result<(), AppError> {
        let problem_id = self.problem.id;
        let subject = Subject::flag_of(problem_id);
        let after = issues_of(self.db(), checker, subject, Reads::Through(&self.txn)).await?;
        let broken_after: Vec<_> = after
            .issues
            .iter()
            .filter_map(ConfigIssue::of_stored)
            .collect();
        if on_break == OnConfigBreak::Refuse && !broken_after.is_empty() {
            let before = issues_of(self.db(), checker, subject, Reads::Committed).await?;
            let broken_before: Vec<_> = before
                .issues
                .iter()
                .filter_map(ConfigIssue::of_stored)
                .collect();
            let caused: Vec<_> = broken_after
                .into_iter()
                .filter(|issue| !broken_before.contains(issue))
                .collect();
            if !caused.is_empty() {
                return Err(AppError::ConfigWouldBreak(caused));
            }
        }
        record_outcome(self.db(), problem_id, &after).await?;
        let txn = Arc::try_unwrap(self.txn).map_err(|_| {
            AppError::Internal("a config check still holds the test data change".into())
        })?;
        txn.commit().await?;
        Ok(())
    }
}
