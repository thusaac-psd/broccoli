//! The server's wall clock, as seen from this machine.
//!
//! The server decides when a contest starts and ends by its own clock, and a
//! contestant's machine often runs minutes or hours off it (no NTP on a lab
//! network, an RTC holding local time). So the CLI never reads this
//! machine's wall clock: it estimates the server's from the `Date` header of
//! the API responses it receives anyway, and counts forward from there with
//! the monotonic clock.
//!
//! `Date` has whole-second resolution and is stamped somewhere between when
//! the request was sent and when the response arrived, so each response
//! bounds the server's time to an interval. Intersecting the intervals of
//! successive responses narrows the estimate. An empty intersection means the
//! server's clock was changed, so the estimate starts over from that response.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, TimeDelta, Utc};

/// The server's time, estimated from response `Date` headers. Clones share
/// one estimate.
#[derive(Debug, Clone, Default)]
pub struct ServerClock(Arc<Mutex<Option<Estimate>>>);

/// Bounds of the server's time at the monotonic instant `base`.
#[derive(Debug, Clone, Copy)]
struct Estimate {
    base: Instant,
    lower: DateTime<Utc>,
    upper: DateTime<Utc>,
}

impl ServerClock {
    /// Feed in a response the server stamped `date`, for a request sent at
    /// `sent` whose response arrived at `received`.
    pub fn observe(&self, sent: Instant, received: Instant, date: DateTime<Utc>) {
        let mut estimate = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let restart = Estimate::sample(received, sent, received, date);
        *estimate = Some(match *estimate {
            Some(e) => e
                .intersect(Estimate::sample(e.base, sent, received, date))
                .unwrap_or(restart),
            None => restart,
        });
    }

    /// Feed in a response's `Date` header, if it has one.
    pub fn observe_header(&self, sent: Instant, received: Instant, date: Option<&str>) {
        if let Some(date) = date.and_then(|d| DateTime::parse_from_rfc2822(d).ok()) {
            self.observe(sent, received, date.with_timezone(&Utc));
        }
    }

    /// The server's time at the monotonic instant `at`, or `None` before any
    /// response carried a `Date`.
    pub fn at(&self, at: Instant) -> Option<DateTime<Utc>> {
        let estimate = (*self.0.lock().unwrap_or_else(|e| e.into_inner()))?;
        let middle = estimate.lower + (estimate.upper - estimate.lower) / 2;
        Some(middle + signed(estimate.base, at))
    }

    /// The server's time now; see [`ServerClock::at`].
    pub fn now(&self) -> Option<DateTime<Utc>> {
        self.at(Instant::now())
    }
}

impl Estimate {
    /// The bounds one response puts on the server's time at `base`: it was
    /// stamped at some instant between `sent` and `received`, while the
    /// server's clock read between `date` and a second later.
    fn sample(base: Instant, sent: Instant, received: Instant, date: DateTime<Utc>) -> Self {
        Self {
            base,
            lower: date - signed(base, received),
            upper: date + TimeDelta::seconds(1) - signed(base, sent),
        }
    }

    fn intersect(self, other: Self) -> Option<Self> {
        let lower = self.lower.max(other.lower);
        let upper = self.upper.min(other.upper);
        (lower <= upper).then_some(Self {
            base: self.base,
            lower,
            upper,
        })
    }
}

/// `at - base`, negative when `at` comes first.
fn signed(base: Instant, at: Instant) -> TimeDelta {
    let delta = |d| TimeDelta::from_std(d).unwrap_or(TimeDelta::MAX);
    match at.checked_duration_since(base) {
        Some(d) => delta(d),
        None => -delta(base - at),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn date(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn no_response_means_no_server_time() {
        assert_eq!(ServerClock::default().now(), None);
    }

    #[test]
    fn the_server_time_counts_forward_monotonically_from_a_response() {
        let t0 = Instant::now();
        let clock = ServerClock::default();
        clock.observe(t0, t0 + ms(100), date("2030-01-01T10:00:00Z"));
        let now = clock.at(t0 + ms(100) + Duration::from_secs(3600)).unwrap();
        // Within the one-second-plus-round-trip window of the sample.
        assert!(now >= date("2030-01-01T11:00:00Z"), "{now}");
        assert!(now <= date("2030-01-01T11:00:01.100Z"), "{now}");
    }

    #[test]
    fn successive_responses_narrow_the_estimate_below_a_second() {
        let t0 = Instant::now();
        let clock = ServerClock::default();
        // The server's clock reads 10:00:00.700 at t0. A response stamped
        // during a 10 ms round trip reads the whole second it was stamped in.
        let true_at = |t: Instant| date("2030-01-01T10:00:00.700Z") + signed(t0, t);
        for sent in [0, 250, 500, 750, 1000, 1250] {
            let sent = t0 + ms(sent);
            let received = sent + ms(10);
            let stamped = true_at(sent + ms(5));
            let whole = DateTime::from_timestamp(stamped.timestamp(), 0).unwrap();
            clock.observe(sent, received, whole);
        }
        let at = t0 + ms(5000);
        let error = (clock.at(at).unwrap() - true_at(at)).abs();
        assert!(error <= TimeDelta::milliseconds(150), "{error}");
    }

    #[test]
    fn a_server_clock_change_restarts_the_estimate() {
        let t0 = Instant::now();
        let clock = ServerClock::default();
        clock.observe(t0, t0 + ms(10), date("2030-01-01T10:00:00Z"));
        clock.observe(t0 + ms(1000), t0 + ms(1010), date("2030-01-01T12:00:01Z"));
        let now = clock.at(t0 + ms(1010)).unwrap();
        assert!(now >= date("2030-01-01T12:00:01Z"), "{now}");
        assert!(now <= date("2030-01-01T12:00:02Z"), "{now}");
    }

    #[test]
    fn the_http_date_header_is_read() {
        let t0 = Instant::now();
        let clock = ServerClock::default();
        clock.observe_header(t0, t0, Some("Tue, 01 Jan 2030 10:00:00 GMT"));
        assert!(clock.now().unwrap() >= date("2030-01-01T10:00:00Z"));
        let missing = ServerClock::default();
        missing.observe_header(t0, t0, Some("not a date"));
        missing.observe_header(t0, t0, None);
        assert_eq!(missing.now(), None);
    }
}
