use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use crate::services::window_quota::Outcome;

/// Upper bound on tracked (username, ip) keys. A distributed attacker spraying
/// unique usernames/IPs could otherwise grow the map without bound. When the cap
/// is hit we GC aged-out keys, and if that is not enough we drop all tracking
/// (fail open) rather than risk memory exhaustion - losing throttle state is
/// always safer than losing the process during a live contest.
const MAX_KEYS: usize = 50_000;

/// Contest-safe brute-force throttle for the login endpoint.
///
/// Keyed on (username, client IP) and counts ONLY failed attempts, which makes
/// it safe to run during a real onsite contest where hundreds of contestants
/// share one venue NAT IP:
/// - a legitimate contestant who signs in is never throttled - success
///   [`clear`](Self::clear)s the key, and other contestants are distinct
///   usernames, so one person's failures never touch another's;
/// - an attacker guessing a victim's password from the attacker's own IP only
///   fills the (victim, attacker-ip) key, so the victim signing in from their
///   own IP is unaffected - there is no lockout-DoS on the victim;
/// - repeated failures for a single (username, ip) back off with a bounded
///   retry-after that always expires within `window`; the endpoint is never
///   permanently denied.
///
/// Like the per-address quota (`services::window_quota`), an attempt is
/// [`check`](Self::check)ed before its password is verified and
/// [`settle`](Self::settle)d after: a failure past the limit and a success
/// while the pair is throttled both answer the throttle instead, so attempts
/// verified in parallel learn at most `limit` failures between them.
///
/// State is per-process and in-memory (it resets on restart). An attacker spread
/// across replicas gets N times the budget - an accepted trade for zero DB load
/// and never risking a persistent lockout mid-contest.
pub struct LoginThrottle {
    limit: u32,
    window: Duration,
    inner: Mutex<HashMap<(String, IpAddr), VecDeque<Instant>>>,
}

impl LoginThrottle {
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            window,
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn key(username: &str, ip: IpAddr) -> (String, IpAddr) {
        // Lowercase so an attacker cannot reset the counter by varying case; the
        // pair is still per-IP, so merging case variants only ever affects the
        // attacker's own key, never a victim's independent login.
        (username.trim().to_ascii_lowercase(), ip)
    }

    /// Returns `Some(retry_after_secs)` when this (username, ip) pair is
    /// currently throttled, `None` otherwise. Always `None` when disabled
    /// (`limit == 0`), so a misconfigured limit fails open.
    pub fn check(&self, username: &str, ip: IpAddr) -> Option<u64> {
        self.check_at(username, ip, Instant::now())
    }

    fn check_at(&self, username: &str, ip: IpAddr, now: Instant) -> Option<u64> {
        if self.limit == 0 {
            return None;
        }
        self.throttled_at(username, ip, now, self.limit)
    }

    /// `Some(retry_after_secs)` while the pair holds at least `threshold`
    /// failures in the window.
    fn throttled_at(
        &self,
        username: &str,
        ip: IpAddr,
        now: Instant,
        threshold: u32,
    ) -> Option<u64> {
        let mut guard = self.inner.lock().ok()?;
        let key = Self::key(username, ip);
        let entry = guard.get_mut(&key)?;
        prune(entry, now, self.window);
        if entry.len() as u32 >= threshold {
            // Locked until the OLDEST failure in the window ages out and frees a
            // slot. Bounded by `window`, so the lock always lifts on its own.
            let oldest = *entry.front()?;
            let secs = (oldest + self.window)
                .saturating_duration_since(now)
                .as_secs()
                .max(1);
            Some(secs)
        } else {
            if entry.is_empty() {
                guard.remove(&key);
            }
            None
        }
    }

    /// Decide a [`check`](Self::check)ed attempt for this (username, ip) by
    /// its outcome. A failure is recorded and is throttled past the limit; a
    /// success is throttled while the pair is, and otherwise clears it so a
    /// contestant who finally types the right password starts clean. Returns
    /// `Some(retry_after_secs)` when the attempt is to be answered with the
    /// throttle instead of its outcome.
    pub fn settle(&self, username: &str, ip: IpAddr, outcome: Outcome) -> Option<u64> {
        self.settle_at(username, ip, outcome, Instant::now())
    }

    fn settle_at(&self, username: &str, ip: IpAddr, outcome: Outcome, now: Instant) -> Option<u64> {
        if self.limit == 0 {
            return None;
        }
        match outcome {
            Outcome::Counts => {
                self.record_failure_at(username, ip, now);
                self.throttled_at(username, ip, now, self.limit + 1)
            }
            Outcome::Free => {
                let throttled = self.check_at(username, ip, now);
                if throttled.is_none()
                    && let Ok(mut guard) = self.inner.lock()
                {
                    guard.remove(&Self::key(username, ip));
                }
                throttled
            }
        }
    }

    fn record_failure_at(&self, username: &str, ip: IpAddr, now: Instant) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        if guard.len() >= MAX_KEYS {
            guard.retain(|_, entry| {
                prune(entry, now, self.window);
                !entry.is_empty()
            });
            if guard.len() >= MAX_KEYS {
                // Still saturated after GC: under a spraying attack. Fail open.
                guard.clear();
            }
        }
        let key = Self::key(username, ip);
        let entry = guard.entry(key).or_default();
        prune(entry, now, self.window);
        entry.push_back(now);
        // The window already bounds this, but cap defensively so a burst inside
        // one window cannot grow a single deque without limit.
        while entry.len() as u32 > self.limit + 1 {
            entry.pop_front();
        }
    }
}

fn prune(entry: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while let Some(&front) = entry.front() {
        if now.saturating_duration_since(front) >= window {
            entry.pop_front();
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn under_the_limit_never_throttles() {
        let t = LoginThrottle::new(3, Duration::from_secs(60));
        let now = Instant::now();
        t.record_failure_at("alice", ip("1.1.1.1"), now);
        t.record_failure_at("alice", ip("1.1.1.1"), now);
        assert_eq!(t.check_at("alice", ip("1.1.1.1"), now), None);
    }

    #[test]
    fn reaching_the_limit_throttles_with_bounded_retry_after() {
        let t = LoginThrottle::new(3, Duration::from_secs(60));
        let now = Instant::now();
        for _ in 0..3 {
            t.record_failure_at("alice", ip("1.1.1.1"), now);
        }
        let retry = t
            .check_at("alice", ip("1.1.1.1"), now)
            .expect("should be throttled at the limit");
        assert!(
            (1..=60).contains(&retry),
            "retry_after within window: {retry}"
        );
    }

    #[test]
    fn throttle_expires_after_the_window() {
        let t = LoginThrottle::new(3, Duration::from_secs(60));
        let now = Instant::now();
        for _ in 0..3 {
            t.record_failure_at("alice", ip("1.1.1.1"), now);
        }
        assert!(t.check_at("alice", ip("1.1.1.1"), now).is_some());
        // After the window the failures age out and the key frees.
        let later = now + Duration::from_secs(61);
        assert_eq!(t.check_at("alice", ip("1.1.1.1"), later), None);
    }

    #[test]
    fn success_clears_the_key() {
        let t = LoginThrottle::new(3, Duration::from_secs(60));
        let now = Instant::now();
        for _ in 0..2 {
            t.record_failure_at("alice", ip("1.1.1.1"), now);
        }
        assert_eq!(
            t.settle_at("alice", ip("1.1.1.1"), Outcome::Free, now),
            None
        );
        for _ in 0..2 {
            t.record_failure_at("alice", ip("1.1.1.1"), now);
        }
        assert_eq!(t.check_at("alice", ip("1.1.1.1"), now), None);
    }

    #[test]
    fn attempts_checked_in_parallel_learn_at_most_the_limit_of_failures() {
        let t = LoginThrottle::new(3, Duration::from_secs(60));
        let now = Instant::now();
        // Ten attempts all pass the check before any of them is verified.
        for _ in 0..10 {
            assert_eq!(t.check_at("alice", ip("1.1.1.1"), now), None);
        }
        let answered: Vec<_> = (0..10)
            .map(|_| t.settle_at("alice", ip("1.1.1.1"), Outcome::Counts, now))
            .collect();
        assert_eq!(answered.iter().filter(|a| a.is_none()).count(), 3);
        // A right password verified while the pair is throttled answers the
        // throttle too, and clears nothing.
        assert!(
            t.settle_at("alice", ip("1.1.1.1"), Outcome::Free, now)
                .is_some()
        );
        assert!(t.check_at("alice", ip("1.1.1.1"), now).is_some());
    }

    #[test]
    fn a_victim_is_not_locked_out_by_an_attacker_on_a_different_ip() {
        // Attacker hammers alice's account from their own IP; alice signing in
        // from her own IP must be completely unaffected (no lockout DoS).
        let t = LoginThrottle::new(3, Duration::from_secs(60));
        let now = Instant::now();
        for _ in 0..10 {
            t.record_failure_at("alice", ip("9.9.9.9"), now);
        }
        assert!(t.check_at("alice", ip("9.9.9.9"), now).is_some());
        assert_eq!(t.check_at("alice", ip("1.1.1.1"), now), None);
    }

    #[test]
    fn contestants_sharing_a_venue_ip_do_not_throttle_each_other() {
        // Same venue NAT IP, different usernames: one person's failures must not
        // throttle another, and a successful contestant is never throttled.
        let t = LoginThrottle::new(3, Duration::from_secs(60));
        let now = Instant::now();
        let venue = ip("203.0.113.7");
        for _ in 0..5 {
            t.record_failure_at("alice", venue, now);
        }
        assert_eq!(t.check_at("bob", venue, now), None);
    }

    #[test]
    fn case_insensitive_username_key() {
        let t = LoginThrottle::new(3, Duration::from_secs(60));
        let now = Instant::now();
        for _ in 0..3 {
            t.record_failure_at("Alice", ip("1.1.1.1"), now);
        }
        assert!(t.check_at("alice", ip("1.1.1.1"), now).is_some());
    }

    #[test]
    fn disabled_when_limit_is_zero() {
        let t = LoginThrottle::new(0, Duration::from_secs(60));
        let now = Instant::now();
        for _ in 0..100 {
            t.record_failure_at("alice", ip("1.1.1.1"), now);
        }
        assert_eq!(t.check_at("alice", ip("1.1.1.1"), now), None);
    }
}
