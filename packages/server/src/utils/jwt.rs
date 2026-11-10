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
    /// Issued-at (unix seconds). `#[serde(default)]` so tokens minted before
    /// this claim existed still decode (as `iat = 0`) on the stateless path;
    /// the `FreshAuthUser` extractor treats such a token as predating any
    /// credential change and re-authenticates it.
    #[serde(default)]
    pub iat: u64,
}

/// Sign a 5-minute access token. `issued_at` comes from the database clock
/// (see `services::auth_tokens`), the same clock that stamps
/// `user.credentials_changed_at`, so `FreshAuthUser`'s `iat` comparison does
/// not depend on clock agreement between server replicas.
pub fn sign_access_token(
    user_id: i32,
    username: &str,
    roles: Vec<String>,
    permissions: Vec<String>,
    issued_at: DateTime<Utc>,
    secret: &str,
) -> Result<String> {
    let expiration = issued_at
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
