//! Score arithmetic every contest and evaluator plugin shares.

/// The sum of `scores`, 0 for none. Every score total goes through this:
/// `Iterator::sum` over `f64` starts from `-0.0`, which an empty total would
/// show as `-0`.
pub fn total_score(scores: impl IntoIterator<Item = f64>) -> f64 {
    scores.into_iter().fold(0.0, |total, score| total + score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_to_add_up_totals_a_positive_zero() {
        let total = total_score(std::iter::empty());
        assert_eq!(total, 0.0);
        assert!(total.is_sign_positive(), "{total}");
        assert_eq!(total_score([0.25, 0.5]), 0.75);
    }
}
