//! The one rule for a database error a host function meets, whatever SQL
//! met it: the host's own (storage, timers, configs, submissions, feedback)
//! or a plugin's ([`super::sql`]).
//!
//! An outage ([`common::db::outage`]) says nothing about what the plugin
//! asked for, so it fails the plugin's call as the database's: a lost
//! database as `DATABASE_UNAVAILABLE` ([`plugin_core::error::PluginError::DatabaseUnavailable`]),
//! a saturated pool as `DATABASE_BUSY` ([`plugin_core::error::PluginError::DatabaseBusy`]).
//! Both are `is_unavailable`, so a route answers a retryable 503, a timer
//! delivery is handed back with its attempt refunded and a hook is skipped
//! or answers 503; under a judging dispatch the outage is the platform's and
//! hands the attempt back (`DispatchFault::DatabaseUnavailable`).
//! Any other error is about the statement and fails the call as the
//! plugin's.
//!
//! A host function hands every database error to [`failed`] or [`settle`]
//! and keeps it as the error's source, never as text: every host function is
//! built by [`super::host_function`], which applies [`settle_call`] to the
//! error it returns, so an outage carried anywhere in the error's chain
//! counts even where the body forgot. `tests/integration/source_invariants.rs`
//! checks that no host function turns an error it met into a message.

use common::db::Outage;
use sea_orm::DbErr;

use broccoli_api_types::error::HostCode;

use crate::error::AppError;
use crate::services::judging::{Dispatch, DispatchFailure, DispatchFault};

/// Fail the running call as `outage` makes it fail: the one mapping from an
/// outage to the call's answer, shared by plugin SQL and the host's own.
pub(crate) fn fail_call(outage: Outage) {
    match outage {
        Outage::Lost => plugin_core::executor::fail_database_unavailable(),
        Outage::Busy => plugin_core::executor::fail_database_busy(),
    }
}

/// The error a host function returns for `e`, a database error its own SQL
/// met: an outage fails the call as the database's (see the module docs);
/// any other error fails it as the host function's. Either way `e` stays the
/// error's source.
pub(crate) fn failed(e: DbErr) -> extism::Error {
    match common::db::outage(&e) {
        Some(outage) => {
            tracing::warn!(error = %e, "A host function's database is unavailable");
            noted(outage, &e.to_string());
            extism::Error::new(e).context("the database is unavailable")
        }
        None => {
            tracing::error!(error = %e, "A host function's statement failed");
            extism::Error::new(e).context("database error")
        }
    }
}

/// `answer` as a host function that reports a statement's own error to the
/// plugin takes it: an outage fails the call ([`failed`]); any other error
/// is the statement's and stays in the answer.
pub(crate) fn settle<T>(answer: Result<T, DbErr>) -> Result<Result<T, DbErr>, extism::Error> {
    match answer {
        Err(e) if common::db::is_outage(&e) => Err(failed(e)),
        answer => Ok(answer),
    }
}

/// The plugin's answer to its statement that failed with `e`, an error of
/// the statement's own (an outage never reaches here: [`settle`] took it).
pub(crate) fn refused(e: &DbErr) -> Result<String, extism::Error> {
    super::sql::HostDbResponse::err(e.to_string()).to_json_string()
}

/// The error a host function returns for `e`, an [`AppError`] a service it
/// called answered: the database's outage fails the call as the database's,
/// as [`failed`] does; any other error is `refused`'s to word.
pub(crate) fn failed_service(
    e: AppError,
    refused: impl FnOnce(AppError) -> extism::Error,
) -> extism::Error {
    let outage = match e {
        AppError::DatabaseUnavailable => Outage::Lost,
        AppError::DatabaseBusy => Outage::Busy,
        other => return refused(other),
    };
    tracing::warn!(?outage, "A host function's database is unavailable");
    noted(outage, "the database is unavailable")
}

/// Apply the rule to `error`, which a host function returned: an outage any
/// [`DbErr`] in its chain reports fails the call as the database's. Marking
/// is idempotent, so an error [`failed`] already settled is settled again
/// harmlessly.
pub(super) fn settle_call(error: &extism::Error) {
    if let Some(outage) = outage_in(error) {
        noted(outage, &format!("{error:#}"));
    }
}

/// Record `outage` on the running call's dispatch, if any, and fail the call
/// as it makes it fail.
fn noted(outage: Outage, message: &str) -> extism::Error {
    fail_under(Dispatch::of_current_call().as_ref(), outage, message);
    extism::Error::msg(message.to_string())
}

/// Fail the running call as `outage` makes it fail and, when it runs under
/// `dispatch`, record the outage there (described by `message`): the
/// database's outage is the platform's, so the attempt is handed back
/// ([`DispatchFault::DatabaseUnavailable`]) instead of spending its budget.
pub(crate) fn fail_under(dispatch: Option<&Dispatch>, outage: Outage, message: &str) {
    if let Some(dispatch) = dispatch {
        dispatch
            .faults()
            .record(DispatchFault::DatabaseUnavailable, message.to_string());
    }
    fail_call(outage);
}

/// What a dispatch's own statement (`what`, as "Failed to {what}") that met
/// `e` fails its attempt with, by the same rule: an outage hands the attempt
/// back ([`DispatchFault::DatabaseUnavailable`]); any other error is a
/// judging failure.
pub(crate) fn dispatch_failed(what: &str, e: &DbErr) -> DispatchFailure {
    let message = format!("Failed to {what}: {e}");
    match common::db::outage(e) {
        Some(_) => DispatchFailure::faulted(DispatchFault::DatabaseUnavailable, message),
        None => DispatchFailure::judging(HostCode::DatabaseError, message),
    }
}

/// The outage a [`DbErr`] in `error`'s chain reports, if one does.
fn outage_in(error: &extism::Error) -> Option<Outage> {
    error
        .chain()
        .find_map(|cause| common::db::outage(cause.downcast_ref::<DbErr>()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lost() -> DbErr {
        DbErr::ConnectionAcquire(sea_orm::error::ConnAcquireErr::Timeout)
    }

    /// The outage found in an error whose chain carries `cause` under a
    /// layer of the host function's own context.
    fn found(cause: extism::Error) -> Option<Outage> {
        outage_in(&cause.context("the host function's own words"))
    }

    #[test]
    fn an_outage_anywhere_in_a_host_functions_error_is_found() {
        assert_eq!(found(extism::Error::new(lost())), Some(Outage::Lost));
        assert_eq!(
            found(extism::Error::new(common::db::pool_busy())),
            Some(Outage::Busy)
        );
        // `failed` keeps the outage as the source of what it returns.
        assert_eq!(outage_in(&failed(lost())), Some(Outage::Lost));
        // A statement's own error, and an error that is not the database's,
        // are not outages.
        assert_eq!(
            found(extism::Error::new(DbErr::Custom("duplicate key".into()))),
            None
        );
        assert_eq!(found(extism::Error::msg("the input is malformed")), None);
    }

    #[test]
    fn settle_keeps_a_statements_own_error_in_the_answer() {
        assert!(matches!(
            settle::<()>(Err(DbErr::Custom("duplicate key".into()))),
            Ok(Err(_))
        ));
        assert!(settle::<()>(Err(lost())).is_err());
        assert!(matches!(settle(Ok(1)), Ok(Ok(1))));
    }

    /// The dispatch's own database errors follow the outage rule: an outage
    /// hands the attempt back under the database's code; any other error is
    /// a judging failure.
    #[test]
    fn a_dispatch_database_error_hands_back_only_an_outage() {
        let lost = dispatch_failed(
            "read the inputs",
            &sea_orm::DbErr::ConnectionAcquire(sea_orm::error::ConnAcquireErr::Timeout),
        );
        assert_eq!(
            (lost.code, lost.class),
            (
                HostCode::DatabaseUnavailable,
                crate::services::judging::SystemErrorClass::HandBack
            )
        );
        let busy = dispatch_failed("read the inputs", &common::db::pool_busy());
        assert_eq!(
            (busy.code, busy.class),
            (
                HostCode::DatabaseUnavailable,
                crate::services::judging::SystemErrorClass::HandBack
            )
        );
        let own = dispatch_failed(
            "read the inputs",
            &sea_orm::DbErr::Custom("syntax error".into()),
        );
        assert_eq!(
            (own.code, own.class),
            (
                HostCode::DatabaseError,
                crate::services::judging::SystemErrorClass::Judging
            )
        );
        assert!(
            own.message.starts_with("Failed to read the inputs: "),
            "{}",
            own.message
        );
    }
}
