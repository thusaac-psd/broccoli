//! Per-client-address quotas on the unauthenticated endpoints a script could
//! hammer: registering. Failed sign-ins are bounded per account and address
//! (`services::failed_sign_ins`), and device codes by what an address leaves
//! pending (`services::device_auth`), not by a rate.
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

    fn window_of(&self, ip: IpAddr) -> String {
        format!("{}:{ip}", self.name)
    }
}
