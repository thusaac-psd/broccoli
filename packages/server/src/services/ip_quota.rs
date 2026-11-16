//! Per-client-address quotas on the unauthenticated endpoints a script could
//! hammer: failed sign-ins and registering. Device codes are bounded by what
//! an address leaves pending (`services::device_auth`), not by a rate.
//!
//! Each quota is a fixed window per address, counted in Redis by
//! [`super::window_quota`].

use std::net::IpAddr;
use std::time::Duration;

use crate::error::AppError;

/// At most `limit` requests from one client address per `window`.
#[derive(Debug, Clone, Copy)]
pub struct IpQuota {
    name: &'static str,
    pub limit: u64,
    pub window: Duration,
}

/// Failed `POST /auth/login` attempts ([`IpQuota::check`] and
/// [`IpQuota::settle`]): a successful sign-in counts nothing, so a venue that
/// shows the server one address for hundreds of contestants signs them all
/// in, while one source guessing passwords across many accounts is capped.
/// The per-account throttle (`utils::login_throttle`) bounds guessing at one
/// account.
pub const FAILED_LOGIN: IpQuota = IpQuota {
    name: "failed-login",
    limit: 100,
    window: Duration::from_secs(60),
};

/// `POST /auth/register`: generous, so a venue behind one NAT can register,
/// while account spam from one source is capped.
pub const REGISTER: IpQuota = IpQuota {
    name: "register",
    limit: 30,
    window: Duration::from_secs(60),
};

impl IpQuota {
    /// Count a request from `ip` and refuse it with `RATE_LIMITED` (and the
    /// seconds until its window closes) past the limit. A request whose client
    /// address is unknown is not counted.
    pub async fn admit(&self, mq: &mq::Mq, ip: Option<IpAddr>) -> Result<(), AppError> {
        let Some(ip) = ip else {
            return Ok(());
        };
        super::window_quota::admit(mq, &self.window_of(ip), self.limit, self.window).await
    }

    /// Refuse a request from `ip` with `RATE_LIMITED` once the window holds
    /// `limit` events, without counting it; the caller
    /// [`settle`](Self::settle)s its outcome. A request whose client address
    /// is unknown is admitted.
    pub async fn check(&self, mq: &mq::Mq, ip: Option<IpAddr>) -> Result<(), AppError> {
        let Some(ip) = ip else {
            return Ok(());
        };
        super::window_quota::check(mq, &self.window_of(ip), self.limit).await
    }

    /// Decide a [`check`](Self::check)ed request from `ip` by its outcome
    /// ([`super::window_quota::settle`]). A request whose client address is
    /// unknown is admitted.
    pub async fn settle(
        &self,
        mq: &mq::Mq,
        ip: Option<IpAddr>,
        outcome: super::window_quota::Outcome,
    ) -> Result<(), AppError> {
        let Some(ip) = ip else {
            return Ok(());
        };
        super::window_quota::settle(mq, &self.window_of(ip), self.limit, self.window, outcome).await
    }

    fn window_of(&self, ip: IpAddr) -> String {
        format!("{}:{ip}", self.name)
    }
}
