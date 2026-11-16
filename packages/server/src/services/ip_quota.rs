//! Per-client-address quotas on the unauthenticated endpoints a script could
//! hammer: failed sign-ins, registering and minting device codes.
//!
//! Each quota is a fixed window counted in Redis (see
//! [`mq::Mq::count_in_window`]), so it holds across the whole fleet behind a
//! round-robin gateway, and its clock is Redis's, so replicas' clocks never
//! matter. A window opens at an address's first request and closes a fixed
//! time later; requests past the limit count but never extend it, so a client
//! retrying in a loop waits out the same window as one that stopped. One
//! round trip per request. When Redis cannot answer, the request is admitted:
//! these quotas guard against abuse, and an outage must not lock a venue out.

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
/// [`IpQuota::record`]): a successful sign-in counts nothing, so a venue that
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

/// `POST /auth/device-code`: never blocks a real CLI sign-in, caps a source
/// flooding the pending-code store.
pub const DEVICE_CODE: IpQuota = IpQuota {
    name: "device-code",
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
        let counted = mq.count_in_window(&self.window_of(ip), self.window).await;
        self.refuse_past(counted, self.limit)
    }

    /// Refuse a request from `ip` with `RATE_LIMITED` once the window holds
    /// `limit` events, without counting it; the caller [`record`](Self::record)s
    /// the ones that count. A request whose client address is unknown is
    /// admitted.
    pub async fn check(&self, mq: &mq::Mq, ip: Option<IpAddr>) -> Result<(), AppError> {
        let Some(ip) = ip else {
            return Ok(());
        };
        let held = mq.window_count(&self.window_of(ip)).await;
        self.refuse_past(held, self.limit.saturating_sub(1))
    }

    /// Count one event from `ip` toward [`check`](Self::check).
    pub async fn record(&self, mq: &mq::Mq, ip: Option<IpAddr>) {
        let Some(ip) = ip else {
            return;
        };
        if let Err(e) = mq.count_in_window(&self.window_of(ip), self.window).await {
            tracing::warn!(quota = self.name, error = %e, "Rate limit count failed");
        }
    }

    fn window_of(&self, ip: IpAddr) -> String {
        format!("{}:{ip}", self.name)
    }

    /// Refuse when the window holds more than `allowed` events; admit when
    /// Redis could not answer.
    fn refuse_past(
        &self,
        window: Result<(u64, Duration), mq::MqError>,
        allowed: u64,
    ) -> Result<(), AppError> {
        match window {
            Ok((count, left)) if count > allowed => Err(AppError::RateLimited {
                retry_after: left.as_millis().div_ceil(1000).max(1) as u64,
            }),
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(quota = self.name, error = %e, "Rate limit check failed; admitting");
                Ok(())
            }
        }
    }
}
