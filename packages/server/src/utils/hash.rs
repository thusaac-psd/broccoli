use std::sync::OnceLock;

use anyhow::Result;
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};

static DUMMY_HASH: OnceLock<String> = OnceLock::new();

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::encode_b64(&rand::random::<[u8; argon2::RECOMMENDED_SALT_LEN]>())
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
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

/// Argon2 runs on its own threads, one per core, never on tokio's blocking
/// pool. It is deliberately slow (tens of milliseconds of CPU): on an async
/// worker it would stall every request scheduled there, and on the blocking
/// pool it would queue behind plugin calls, so a plugin flood would block
/// login. Jobs beyond the threads wait in the channel, holding no thread. A
/// job that panics ends that job, not its thread.
static PASSWORD_POOL: std::sync::LazyLock<std::sync::mpsc::Sender<PasswordJob>> =
    std::sync::LazyLock::new(|| {
        let (tx, rx) = std::sync::mpsc::channel::<PasswordJob>();
        let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        for _ in 0..threads {
            let rx = rx.clone();
            std::thread::Builder::new()
                .name("password-hash".into())
                .spawn(move || {
                    loop {
                        let job = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                        match job {
                            // The job's result sender drops with it, so
                            // its caller gets an error.
                            Ok(job) => {
                                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                            }
                            Err(_) => return,
                        }
                    }
                })
                .expect("spawn a password hashing thread");
        }
        tx
    });

type PasswordJob = Box<dyn FnOnce() + Send>;

/// Run `work` on the password pool and await its result.
async fn on_password_pool<R: Send + 'static>(
    work: impl FnOnce() -> R + Send + 'static,
) -> Result<R> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    PASSWORD_POOL
        .send(Box::new(move || {
            let _ = tx.send(work());
        }))
        .map_err(|_| anyhow::anyhow!("password pool is gone"))?;
    rx.await
        .map_err(|_| anyhow::anyhow!("password task failed"))
}

/// [`verify_password`] on the password pool.
pub async fn verify_password_off_thread(
    password: String,
    password_hash: Option<String>,
) -> Result<bool> {
    on_password_pool(move || verify_password(&password, password_hash.as_deref())).await?
}

/// [`hash_password`] on the password pool.
pub async fn hash_password_off_thread(password: String) -> Result<String> {
    on_password_pool(move || hash_password(&password)).await?
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_panicking_job_fails_its_caller_and_keeps_its_thread() {
        // One panic per pool thread: a pool that lost a thread per panic
        // would have none left, and the next job would fail to send.
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        for _ in 0..threads {
            assert!(
                on_password_pool(|| panic!("bug in a password job"))
                    .await
                    .is_err()
            );
        }
        assert_eq!(on_password_pool(|| 7).await.unwrap(), 7);
    }

    #[test]
    fn password_checks_run_while_the_blocking_pool_is_exhausted() {
        // A runtime whose only blocking thread is taken, as plugin calls
        // take every blocking thread under a flood.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (release, held) = std::sync::mpsc::channel::<()>();
            let (started_tx, started) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = started_tx.send(());
                let _ = held.recv();
            });
            started.await.unwrap();
            let hash = hash_password_off_thread("secret".into()).await.unwrap();
            assert!(
                verify_password_off_thread("secret".into(), Some(hash))
                    .await
                    .unwrap()
            );
            release.send(()).unwrap();
            blocker.await.unwrap();
        });
    }
}
