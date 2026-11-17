//! The failed sign-in quota: at most `auth.login_failure_limit` failed
//! sign-ins for one account from one client address in any
//! `auth.login_failure_window_secs`, counted across the fleet in a sliding
//! log in Redis ([`mq::Mq::sliding_log`]).
//!
//! It is keyed by (address, account) because that is what guessing leaves
//! behind and a venue does not: contestants behind one NAT address each sign
//! in to their own account, so one contestant's typos never refuse another's
//! sign-in, while guessing one account from one address is bounded. Guessing
//! across accounts is bounded per account the same way; an address-wide count
//! would let a venue's typos lock every contestant out.
//!
//! The bound holds however many attempts run at once. Each attempt is
//! [`check`](FailedSignIns::check)ed before its password is verified and
//! [`settle`](FailedSignIns::settle)d after, before it is answered. A failure
//! takes its place in the log only while the log has room, and is answered
//! 429 when it has none; a success is answered 429 while its own log is full
//! and otherwise clears it. So attempts on one account from one address learn
//! at most `limit` failures in any window-long span, deterministically, and
//! the window slides, so no burst across a window boundary learns more.
//!
//! When Redis cannot answer, the attempt is admitted: the quota guards
//! against guessing, and an outage must not lock a venue out. A request whose
//! client address is unknown is not counted.

use std::net::IpAddr;
use std::time::Duration;

use mq::{LogAnswer, LogOp};

use crate::config::AuthConfig;
use crate::error::AppError;

/// The failed sign-in quota of `auth` (see the module docs).
#[derive(Debug, Clone, Copy)]
pub struct FailedSignIns {
    limit: u64,
    window: Duration,
}

impl FailedSignIns {
    pub fn of(auth: &AuthConfig) -> Self {
        Self {
            limit: u64::from(auth.login_failure_limit),
            window: Duration::from_secs(auth.login_failure_window_secs.max(1)),
        }
    }

    /// Refuse an attempt on `account` (its username key) from `ip` while its
    /// log is full, before its password is verified.
    pub async fn check(
        &self,
        mq: &mq::Mq,
        ip: Option<IpAddr>,
        account: &str,
    ) -> Result<(), AppError> {
        self.apply(mq, ip, account, LogOp::Check).await
    }

    /// Decide a [`check`](Self::check)ed attempt by whether its password
    /// `verified`: a failure is recorded, and refused when the log is full; a
    /// success is refused while the log is full, and otherwise clears it.
    /// The caller answers a refusal instead of the outcome.
    pub async fn settle(
        &self,
        mq: &mq::Mq,
        ip: Option<IpAddr>,
        account: &str,
        verified: bool,
    ) -> Result<(), AppError> {
        let op = if verified { LogOp::Clear } else { LogOp::Count };
        self.apply(mq, ip, account, op).await
    }

    async fn apply(
        &self,
        mq: &mq::Mq,
        ip: Option<IpAddr>,
        account: &str,
        op: LogOp,
    ) -> Result<(), AppError> {
        let Some(ip) = ip else {
            return Ok(());
        };
        if self.limit == 0 {
            return Ok(());
        }
        let log = format!("failed-sign-in:{ip}:{account}");
        match mq.sliding_log(&log, self.limit, self.window, op).await {
            Ok(LogAnswer::Admitted) => Ok(()),
            Ok(LogAnswer::Full { wait }) => Err(AppError::RateLimited {
                retry_after: wait.as_millis().div_ceil(1000).max(1) as u64,
            }),
            Err(e) => {
                tracing::warn!(error = %e, "Failed sign-in quota could not be read; admitting");
                Ok(())
            }
        }
    }
}
