//! The CLI's login session: the token pair one process uses, and the single
//! place that rotates it.
//!
//! ```text
//!            401, refresh token rotated           server refused refresh token
//!   Active ───────────► Refreshing ─────────────────────────────► Rejected
//!     ▲                    │  │                                       │
//!     └── rotated/adopted ─┘  └─ server unreachable ─► Unavailable    │
//!     ▲                                               { retry_at }    │
//!     └──────────── newer pair found in credentials.json ◄────────────┘
//! ```
//!
//! * `Active`: requests carry the access token.
//! * `Refreshing`: one thread exchanges the refresh token; the others wait
//!   for its outcome instead of spending the single-use refresh token again.
//! * `Unavailable`: the last exchange could not reach the server. Requests
//!   fail fast with [`SessionError::Unavailable`] until `retry_at`, so a
//!   server outage costs one refresh attempt per [`REFRESH_RETRY`] instead of
//!   one per request.
//! * `Rejected`: the server refused the refresh token (or there is none).
//!   Requests fail with [`SessionError::Rejected`] without touching the
//!   network, until a new login is saved to `credentials.json`.
//!
//! A stored session also coordinates with other processes: the exchange runs
//! under the credentials file lock and starts by re-reading the file, so a
//! process that lost the race adopts the pair the winner saved instead of
//! replaying the rotated refresh token.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::credentials::{CredentialsFile, StoredTokens};

/// How long a session waits after an unreachable refresh before trying again.
pub const REFRESH_RETRY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Active,
    Refreshing,
    Unavailable { retry_at: Instant },
    Rejected,
}

/// Why a session cannot authorize a request right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// The session ended; the user must log in again.
    Rejected,
    /// The server could not be reached to refresh the session.
    Unavailable { retry_at: Instant, cause: String },
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected => {
                f.write_str("Session expired. Run `broccoli login` to re-authenticate.")
            }
            Self::Unavailable { cause, .. } => {
                write!(f, "Could not refresh the session: {cause}")
            }
        }
    }
}

impl std::error::Error for SessionError {}

/// Outcome of presenting a refresh token to the server.
#[derive(Debug)]
pub enum Exchange {
    Rotated(StoredTokens),
    /// The server refused the refresh token.
    Refused,
    /// The server could not be reached or failed; the token may still be good.
    Unreachable(String),
}

struct Slot {
    token: String,
    refresh_token: Option<String>,
    state: SessionState,
}

struct Inner {
    server: String,
    store: Option<CredentialsFile>,
    slot: Mutex<Slot>,
    settled: Condvar,
}

/// A cloneable handle; clones share one token pair and one state.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

impl Session {
    /// A session backed by the pair saved for `server` in `store`.
    pub fn stored(server: String, tokens: StoredTokens, store: CredentialsFile) -> Self {
        Self::build(server, tokens.token, Some(tokens.refresh_token), Some(store))
    }

    /// A session for an access token given on the command line or in the
    /// environment: it is never refreshed or saved.
    pub fn explicit(server: String, token: String) -> Self {
        Self::build(server, token, None, None)
    }

    fn build(
        server: String,
        token: String,
        refresh_token: Option<String>,
        store: Option<CredentialsFile>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                server,
                store,
                slot: Mutex::new(Slot {
                    token,
                    refresh_token,
                    state: SessionState::Active,
                }),
                settled: Condvar::new(),
            }),
        }
    }

    pub fn server(&self) -> &str {
        &self.inner.server
    }

    pub fn state(&self) -> SessionState {
        self.lock().state
    }

    /// The access token to send, or why no request can be sent now.
    pub fn bearer(&self) -> Result<String, SessionError> {
        self.bearer_at(Instant::now())
    }

    /// Refresh after the server answered 401 to a request that carried
    /// `used`. Returns once a request may be retried with [`Self::bearer`].
    pub fn refresh(
        &self,
        used: &str,
        exchange: impl FnOnce(&str) -> Exchange,
    ) -> Result<(), SessionError> {
        self.refresh_at(used, exchange, Instant::now())
    }

    fn bearer_at(&self, now: Instant) -> Result<String, SessionError> {
        let mut slot = self.lock();
        loop {
            match slot.state {
                SessionState::Active => return Ok(slot.token.clone()),
                SessionState::Refreshing => slot = self.wait(slot),
                SessionState::Unavailable { retry_at } if now < retry_at => {
                    return Err(SessionError::Unavailable {
                        retry_at,
                        cause: "the server was unreachable".into(),
                    });
                }
                SessionState::Unavailable { .. } => return Ok(slot.token.clone()),
                SessionState::Rejected => {
                    return match self.saved_pair_other_than(&slot.token) {
                        Some(tokens) => {
                            adopt(&mut slot, tokens);
                            Ok(slot.token.clone())
                        }
                        None => Err(SessionError::Rejected),
                    };
                }
            }
        }
    }

    fn refresh_at(
        &self,
        used: &str,
        exchange: impl FnOnce(&str) -> Exchange,
        now: Instant,
    ) -> Result<(), SessionError> {
        let mut slot = self.lock();
        loop {
            if slot.token != used && slot.state != SessionState::Refreshing {
                // Another thread already replaced the token this request used.
                return Ok(());
            }
            match slot.state {
                SessionState::Refreshing => slot = self.wait(slot),
                SessionState::Rejected => return Err(SessionError::Rejected),
                SessionState::Unavailable { retry_at } if now < retry_at => {
                    return Err(SessionError::Unavailable {
                        retry_at,
                        cause: "the server was unreachable".into(),
                    });
                }
                SessionState::Active | SessionState::Unavailable { .. } => break,
            }
        }
        slot.state = SessionState::Refreshing;
        let refresh_token = slot.refresh_token.clone();
        drop(slot);

        let outcome = self.rotate(used, refresh_token, exchange);

        let mut slot = self.lock();
        let result = match outcome {
            Exchange::Rotated(tokens) => {
                adopt(&mut slot, tokens);
                Ok(())
            }
            Exchange::Refused => {
                slot.state = SessionState::Rejected;
                Err(SessionError::Rejected)
            }
            Exchange::Unreachable(cause) => {
                let retry_at = now + REFRESH_RETRY;
                slot.state = SessionState::Unavailable { retry_at };
                Err(SessionError::Unavailable { retry_at, cause })
            }
        };
        drop(slot);
        self.inner.settled.notify_all();
        result
    }

    /// Exchange the refresh token, or adopt the pair another process saved.
    fn rotate(
        &self,
        used: &str,
        refresh_token: Option<String>,
        exchange: impl FnOnce(&str) -> Exchange,
    ) -> Exchange {
        let Some(store) = &self.inner.store else {
            return match refresh_token {
                Some(refresh_token) => exchange(&refresh_token),
                None => Exchange::Refused,
            };
        };
        let lock = match store.lock() {
            Ok(lock) => lock,
            Err(e) => return Exchange::Unreachable(format!("{e:#}")),
        };
        let saved = match lock.read(&self.inner.server) {
            Ok(saved) => saved,
            Err(e) => return Exchange::Unreachable(format!("{e:#}")),
        };
        let refresh_token = match saved {
            // Another process rotated (or logged in again) while this request
            // was in flight: its pair is current and ours is spent.
            Some(saved) if saved.token != used => return Exchange::Rotated(saved),
            Some(saved) => saved.refresh_token,
            None => match refresh_token {
                Some(refresh_token) => refresh_token,
                None => return Exchange::Refused,
            },
        };
        let outcome = exchange(&refresh_token);
        if let Exchange::Rotated(tokens) = &outcome {
            // The new pair stays usable in memory even if saving fails; the
            // next login repairs the file.
            let _ = lock.write(&self.inner.server, tokens);
        }
        outcome
    }

    /// A pair saved for this server by a later login, if any.
    fn saved_pair_other_than(&self, token: &str) -> Option<StoredTokens> {
        let store = self.inner.store.as_ref()?;
        store
            .read(&self.inner.server)
            .ok()
            .flatten()
            .filter(|saved| saved.token != token)
    }

    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.inner.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait<'a>(&self, slot: MutexGuard<'a, Slot>) -> MutexGuard<'a, Slot> {
        self.inner
            .settled
            .wait(slot)
            .unwrap_or_else(|e| e.into_inner())
    }
}

fn adopt(slot: &mut Slot, tokens: StoredTokens) {
    slot.token = tokens.token;
    slot.refresh_token = Some(tokens.refresh_token);
    slot.state = SessionState::Active;
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: &str = "https://judge.example";

    fn pair(token: &str, refresh: &str) -> StoredTokens {
        StoredTokens {
            token: token.into(),
            refresh_token: refresh.into(),
        }
    }

    fn store() -> (tempfile::TempDir, CredentialsFile) {
        let dir = tempfile::tempdir().unwrap();
        let file = CredentialsFile::at(dir.path().join("credentials.json"));
        (dir, file)
    }

    /// One CLI process: a session over the pair currently saved in `file`.
    fn process(file: &CredentialsFile) -> Session {
        let tokens = file.read(SERVER).unwrap().unwrap();
        Session::stored(SERVER.into(), tokens, file.clone())
    }

    fn never(_: &str) -> Exchange {
        panic!("the refresh token must not be spent again")
    }

    #[test]
    fn a_rotation_saves_the_new_pair_whole() {
        let (_dir, file) = store();
        file.lock().unwrap().write(SERVER, &pair("t0", "r0")).unwrap();
        let session = process(&file);

        session
            .refresh("t0", |refresh| {
                assert_eq!(refresh, "r0");
                Exchange::Rotated(pair("t1", "r1"))
            })
            .unwrap();

        assert_eq!(session.bearer().unwrap(), "t1");
        assert_eq!(file.read(SERVER).unwrap(), Some(pair("t1", "r1")));
    }

    #[test]
    fn a_process_that_lost_the_race_adopts_the_saved_pair() {
        let (_dir, file) = store();
        file.lock().unwrap().write(SERVER, &pair("t0", "r0")).unwrap();
        let watch = process(&file);
        let submit = process(&file);

        watch
            .refresh("t0", |_| Exchange::Rotated(pair("t1", "r1")))
            .unwrap();
        // `submit` still holds t0 and r0, which the server has rotated away.
        submit.refresh("t0", never).unwrap();

        assert_eq!(submit.bearer().unwrap(), "t1");
    }

    #[test]
    fn threads_that_hit_401_together_rotate_once() {
        let (_dir, file) = store();
        file.lock().unwrap().write(SERVER, &pair("t0", "r0")).unwrap();
        let session = process(&file);
        session
            .refresh("t0", |_| Exchange::Rotated(pair("t1", "r1")))
            .unwrap();
        // A second thread whose request also carried t0 reuses the rotation.
        session.refresh("t0", never).unwrap();
        assert_eq!(session.bearer().unwrap(), "t1");
    }

    #[test]
    fn a_refused_refresh_rejects_the_session_without_further_requests() {
        let (_dir, file) = store();
        file.lock().unwrap().write(SERVER, &pair("t0", "r0")).unwrap();
        let session = process(&file);

        assert_eq!(
            session.refresh("t0", |_| Exchange::Refused),
            Err(SessionError::Rejected)
        );
        assert_eq!(session.state(), SessionState::Rejected);
        assert_eq!(session.bearer(), Err(SessionError::Rejected));
        assert_eq!(session.refresh("t0", never), Err(SessionError::Rejected));
    }

    #[test]
    fn a_rejected_session_resumes_with_a_newer_login() {
        let (_dir, file) = store();
        file.lock().unwrap().write(SERVER, &pair("t0", "r0")).unwrap();
        let session = process(&file);
        let _ = session.refresh("t0", |_| Exchange::Refused);

        file.lock()
            .unwrap()
            .write(SERVER, &pair("fresh", "rf"))
            .unwrap();

        assert_eq!(session.bearer().unwrap(), "fresh");
        assert_eq!(session.state(), SessionState::Active);
    }

    #[test]
    fn an_unreachable_server_is_retried_only_after_the_backoff() {
        let session = Session::explicit(SERVER.into(), "t0".into());
        // Explicit sessions have no refresh token; give this one a store-less
        // refresh token to exercise the network path.
        session.lock().refresh_token = Some("r0".into());
        let t = Instant::now();

        let err = session
            .refresh_at("t0", |_| Exchange::Unreachable("connection refused".into()), t)
            .unwrap_err();
        assert!(matches!(err, SessionError::Unavailable { retry_at, .. } if retry_at == t + REFRESH_RETRY));

        let before = t + REFRESH_RETRY - Duration::from_millis(1);
        assert!(session.bearer_at(before).is_err());
        assert!(session.refresh_at("t0", never, before).is_err());

        let after = t + REFRESH_RETRY;
        assert_eq!(session.bearer_at(after).unwrap(), "t0");
        session
            .refresh_at("t0", |_| Exchange::Rotated(pair("t1", "r1")), after)
            .unwrap();
        assert_eq!(session.bearer_at(after).unwrap(), "t1");
    }

    #[test]
    fn an_explicit_token_is_never_refreshed() {
        let session = Session::explicit(SERVER.into(), "t0".into());
        assert_eq!(session.refresh("t0", never), Err(SessionError::Rejected));
    }
}
