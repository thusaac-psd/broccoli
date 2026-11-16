use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use crate::utils::hash::generate_random_string;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub jti: String,
    pub sub: String,
    pub uid: i32,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub exp: u64,
    /// Issued-at (unix seconds), compared against the user's
    /// `credentials_changed_at` by the `FreshAuthUser` extractor.
    pub iat: u64,
}

/// How long an access token is valid. Access tokens are checked without a
/// session lookup (the read path makes no database call to authenticate), so
/// logging out, which revokes the refresh token, leaves an access token already
/// issued usable for at most this long. A credential change (password, roles,
/// deactivation) ends it at once on every `FreshAuthUser` endpoint.
pub const ACCESS_TOKEN_TTL: Duration = Duration::minutes(5);

/// Sign an access token valid for [`ACCESS_TOKEN_TTL`] from `issued_at`.
///
/// `issued_at` is the database clock (see `services::auth_tokens`), which is
/// also the platform clock every replica verifies `exp` against, and the
/// clock that stamps `user.credentials_changed_at` for `FreshAuthUser`.
pub fn sign_access_token(
    user_id: i32,
    username: &str,
    roles: Vec<String>,
    permissions: Vec<String>,
    issued_at: DateTime<Utc>,
    secret: &str,
) -> Result<String> {
    let expiration = (issued_at + ACCESS_TOKEN_TTL).timestamp();

    let claims = Claims {
        jti: generate_random_string(),
        sub: username.to_owned(),
        uid: user_id,
        roles,
        permissions,
        exp: expiration.max(0) as u64,
        iat: issued_at.timestamp().max(0) as u64,
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?;

    Ok(token)
}

/// How far past `now` a token's `iat` may lie. Every replica signs and
/// verifies with the platform clock, so a later `iat` comes only from a key
/// holder minting tokens dated in the future, which would otherwise outlive
/// every credential change made before that date (`FreshAuthUser` compares
/// `iat` with `credentials_changed_at`).
pub const IAT_LEEWAY_SECS: i64 = 30;

/// Verify `token`'s signature, that it has not expired at `now`, the platform
/// clock, and that it was not issued after `now` (plus [`IAT_LEEWAY_SECS`]).
/// The library's own expiry check reads this host's clock, so it is off and
/// `exp` is compared here.
pub fn verify(token: &str, secret: &str, now: DateTime<Utc>) -> Result<Claims> {
    let mut validation = Validation::default();
    validation.validate_exp = false;
    let claims = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )?
    .claims;
    anyhow::ensure!(
        (claims.exp as i64) > now.timestamp(),
        "access token expired"
    );
    anyhow::ensure!(
        (claims.iat as i64) <= now.timestamp() + IAT_LEEWAY_SECS,
        "access token issued in the future"
    );
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn expiry_is_judged_against_the_given_platform_time_not_this_host_clock() {
        // Issued in 2001, far from this host's clock in either direction.
        let issued = at(1_000_000_000);
        let token = sign_access_token(1, "u", vec![], vec![], issued, "secret").unwrap();
        let claims = verify(&token, "secret", issued + Duration::minutes(4)).unwrap();
        assert_eq!(claims.iat, issued.timestamp() as u64);
        assert!(verify(&token, "secret", issued + ACCESS_TOKEN_TTL).is_err());
    }

    #[test]
    fn a_token_signed_with_another_secret_is_rejected() {
        let issued = at(1_000_000_000);
        let token = sign_access_token(1, "u", vec![], vec![], issued, "secret").unwrap();
        assert!(verify(&token, "other", issued).is_err());
    }

    #[test]
    fn a_token_issued_in_the_future_is_rejected() {
        let now = at(1_000_000_000);
        let token = |issued| sign_access_token(1, "u", vec![], vec![], issued, "secret").unwrap();
        let near = token(now + Duration::seconds(IAT_LEEWAY_SECS));
        assert!(verify(&near, "secret", now).is_ok());
        let ahead = token(now + Duration::seconds(IAT_LEEWAY_SECS + 1));
        assert!(verify(&ahead, "secret", now).is_err());
    }
}
