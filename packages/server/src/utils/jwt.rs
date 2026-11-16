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

/// Sign a 5-minute access token.
///
/// The two timestamps deliberately use different clocks, each matching the
/// one it is compared against:
/// - `iat` is `issued_at`, from the database clock (see
///   `services::auth_tokens`). `FreshAuthUser` compares it with
///   `user.credentials_changed_at`, which the database stamps.
/// - `exp` is this server's clock plus five minutes. Every replica checks it
///   against its own clock, so deriving it from the database clock would hand
///   out already-expired tokens on a server running ahead of the database.
pub fn sign_access_token(
    user_id: i32,
    username: &str,
    roles: Vec<String>,
    permissions: Vec<String>,
    issued_at: DateTime<Utc>,
    secret: &str,
) -> Result<String> {
    let expiration = Utc::now()
        .checked_add_signed(Duration::minutes(5))
        .expect("valid timestamp")
        .timestamp();

    let claims = Claims {
        jti: generate_random_string(),
        sub: username.to_owned(),
        uid: user_id,
        roles,
        permissions,
        exp: expiration as u64,
        iat: issued_at.timestamp().max(0) as u64,
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?;

    Ok(token)
}

pub fn verify(token: &str, secret: &str) -> Result<Claims> {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )?;
    Ok(token_data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server whose clock runs an hour ahead of the database still issues
    /// tokens it accepts itself.
    #[test]
    fn expiry_follows_the_local_clock_not_the_database_clock() {
        let db_now = Utc::now() - Duration::hours(1);
        let token = sign_access_token(1, "u", vec![], vec![], db_now, "secret").unwrap();
        let claims = verify(&token, "secret").expect("token must not be born expired");
        assert_eq!(claims.iat, db_now.timestamp() as u64);
    }
}
