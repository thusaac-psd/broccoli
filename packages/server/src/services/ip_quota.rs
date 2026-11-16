//! Per-client-address quotas on the unauthenticated endpoints a script could
//! hammer: signing in, registering and minting device codes.
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

/// `POST /auth/login` (with `server.rate_limit_auth`).
pub const LOGIN: IpQuota = IpQuota {
    name: "login",
    limit: 10,
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
        match mq
            .count_in_window(&format!("{}:{ip}", self.name), self.window)
            .await
        {
            Ok((count, left)) if count > self.limit => Err(AppError::RateLimited {
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
