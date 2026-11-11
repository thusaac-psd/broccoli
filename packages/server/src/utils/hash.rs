use std::sync::OnceLock;

use anyhow::Result;
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};

static DUMMY_HASH: OnceLock<String> = OnceLock::new();

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();

    let password_hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?
        .to_string();

    Ok(password_hash)
}

pub fn get_dummy_hash() -> &'static str {
    DUMMY_HASH.get_or_init(|| {
        hash_password("__broccoli_dummy__").expect("Failed to pre-compute dummy password hash")
    })
}

pub fn verify_password(password: &str, password_hash: Option<&str>) -> Result<bool> {
    let hash_to_verify = match password_hash {
        Some(hash) => hash,
        None => get_dummy_hash(),
    };

    let parsed_hash =
        PasswordHash::new(hash_to_verify).map_err(|e| anyhow::anyhow!(e.to_string()))?;

    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok())
}

pub fn generate_random_string() -> String {
    let mut key = [0u8; 32];
    rand::fill(&mut key);
    hex::encode(key)
}

/// [`verify_password`] on the blocking pool. Argon2 is deliberately slow
/// (tens of milliseconds of CPU); running it on an async worker thread stalls
/// every other request scheduled there, which a login storm at contest start
/// turns into runtime-wide latency.
pub async fn verify_password_off_thread(
    password: String,
    password_hash: Option<String>,
) -> Result<bool> {
    tokio::task::spawn_blocking(move || verify_password(&password, password_hash.as_deref()))
        .await
        .map_err(|e| anyhow::anyhow!("password verification task failed: {e}"))?
}

/// [`hash_password`] on the blocking pool; see [`verify_password_off_thread`].
pub async fn hash_password_off_thread(password: String) -> Result<String> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| anyhow::anyhow!("password hashing task failed: {e}"))?
}

/// Lowercase hex SHA-256 of `input`.
pub fn sha256_hex(input: impl AsRef<[u8]>) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(input.as_ref()))
}

/// Byte comparison whose running time does not depend on where the inputs
/// first differ.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
