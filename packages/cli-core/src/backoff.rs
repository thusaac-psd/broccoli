//! Exponential backoff shared by the CLI's retry loops.

use std::time::Duration;

/// `base * 2^attempt`, never more than `cap`.
pub fn exponential(base: Duration, attempt: u32, cap: Duration) -> Duration {
    2u32.checked_pow(attempt)
        .and_then(|factor| base.checked_mul(factor))
        .map_or(cap, |delay| delay.min(cap))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_per_attempt_up_to_the_cap() {
        let s = Duration::from_secs;
        assert_eq!(exponential(s(2), 0, s(30)), s(2));
        assert_eq!(exponential(s(2), 3, s(30)), s(16));
        assert_eq!(exponential(s(2), 4, s(30)), s(30));
        assert_eq!(exponential(s(2), 40, s(30)), s(30));
    }
}
