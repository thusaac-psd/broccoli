//! Raise the process's open-file limit at startup.
//!
//! Linux starts services with a SOFT `RLIMIT_NOFILE` of 1024 - both Docker and
//! systemd's defaults - while the HARD limit is typically 524288. A server holds
//! an fd per client connection, per pooled DB/Redis connection and per opened
//! plugin `.wasm`, so 1024 is exhausted by a few hundred concurrent clients.
//! Measured on a real stack: ~500 spectators polling the bracket drove the
//! server into `EMFILE`, after which TCP accepts, Redis heartbeats and plugin
//! instance rebuilds all failed - and the fail-closed visibility kernel turned
//! the failed plugin calls into 404s, so a live contest intermittently "did not
//! exist". Raising soft to hard is what Go's runtime does automatically; Rust
//! does not, so each long-running binary calls [`raise_nofile_limit`] once.

/// Outcome of [`raise_nofile_limit`], for the caller to log once tracing is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NofileRaise {
    /// Soft limit raised from `from` to `to` (the hard limit).
    Raised { from: u64, to: u64 },
    /// Soft limit already equals the hard limit; nothing to do.
    AlreadyAtHard { limit: u64 },
    /// `getrlimit`/`setrlimit` failed; the process keeps its current limit.
    Failed { errno: i32 },
    /// Not a Unix target.
    Unsupported,
}

impl NofileRaise {
    /// Report the outcome. Call once tracing is initialized.
    pub fn log(self) {
        match self {
            Self::Raised { from, to } => {
                tracing::info!(from, to, "Raised open-file soft limit to the hard limit")
            }
            Self::AlreadyAtHard { limit } => {
                tracing::info!(limit, "Open-file soft limit already at the hard limit")
            }
            Self::Failed { errno } => tracing::warn!(
                errno,
                "Could not raise the open-file soft limit; the process may run out of \
                 file descriptors (EMFILE) under a few hundred concurrent connections"
            ),
            Self::Unsupported => {}
        }
    }
}

/// Raise the soft `RLIMIT_NOFILE` to the hard limit. Never lowers anything and
/// never needs privileges (raising soft up to hard is always permitted).
#[cfg(unix)]
// `rlim_t` is `u64` on 64-bit Linux but not on every Unix target, so the
// `as u64` casts below are only redundant on some platforms.
#[allow(clippy::unnecessary_cast)]
pub fn raise_nofile_limit() -> NofileRaise {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `lim` is a valid, writable `rlimit`.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return NofileRaise::Failed {
            errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        };
    }
    let from = lim.rlim_cur as u64;
    if lim.rlim_cur >= lim.rlim_max {
        return NofileRaise::AlreadyAtHard { limit: from };
    }
    let target = lim.rlim_max;
    // macOS rejects a soft limit above OPEN_MAX even when the hard limit is
    // RLIM_INFINITY; cap there so the call succeeds instead of failing whole.
    #[cfg(target_os = "macos")]
    let target = target.min(libc::OPEN_MAX as libc::rlim_t);
    lim.rlim_cur = target;
    // SAFETY: `lim` is a valid `rlimit` with soft <= hard.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } != 0 {
        return NofileRaise::Failed {
            errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        };
    }
    NofileRaise::Raised {
        from,
        to: target as u64,
    }
}

#[cfg(not(unix))]
pub fn raise_nofile_limit() -> NofileRaise {
    NofileRaise::Unsupported
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[allow(clippy::unnecessary_cast)] // see raise_nofile_limit
    fn current() -> (u64, u64) {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: valid, writable rlimit.
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) }, 0);
        (lim.rlim_cur as u64, lim.rlim_max as u64)
    }

    #[test]
    fn leaves_the_soft_limit_at_the_hard_limit_and_never_lowers_it() {
        let (soft_before, hard) = current();
        let outcome = raise_nofile_limit();
        let (soft_after, hard_after) = current();
        assert_eq!(hard_after, hard, "the hard limit must never change");
        assert!(soft_after >= soft_before, "must never lower the soft limit");
        match outcome {
            NofileRaise::Raised { from, to } => {
                assert_eq!(from, soft_before);
                assert_eq!(to, soft_after);
            }
            NofileRaise::AlreadyAtHard { limit } => assert_eq!(limit, soft_after),
            other => panic!("unexpected outcome {other:?}"),
        }
        // Idempotent: a second call finds nothing to do.
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            raise_nofile_limit(),
            NofileRaise::AlreadyAtHard { limit: soft_after }
        );
    }
}
