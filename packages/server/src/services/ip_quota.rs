//! Per-client-address quotas on the unauthenticated endpoints a script could
//! hammer: signing in, registering and minting device codes.
//!
//! Each quota is a fixed window per address, counted in Redis by
//! [`super::window_quota::admit`].

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
        super::window_quota::admit(mq, &format!("{}:{ip}", self.name), self.limit, self.window)
            .await
    }
}
