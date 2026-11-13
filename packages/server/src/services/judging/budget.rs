/// A bounded number of retries. The one rule every retry budget uses:
/// another retry is allowed while fewer than `max` were spent, so a budget of
/// `n` allows exactly `n` retries after the first attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RetryBudget {
    max: i32,
}

impl RetryBudget {
    pub(crate) fn new(max_retries: u32) -> Self {
        Self {
            max: i32::try_from(max_retries).unwrap_or(i32::MAX),
        }
    }

    /// Whether a unit that already spent `spent` retries may retry again.
    pub(crate) fn allows(self, spent: i32) -> bool {
        spent < self.max
    }

    /// The bound for SQL: a row may retry again while `column < max`.
    pub(crate) fn max(self) -> i32 {
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::RetryBudget;

    #[test]
    fn a_budget_of_n_allows_exactly_n_retries() {
        let budget = RetryBudget::new(2);
        assert!(budget.allows(0));
        assert!(budget.allows(1));
        assert!(!budget.allows(2));
        assert!(!RetryBudget::new(0).allows(0));
        assert_eq!(RetryBudget::new(u32::MAX).max(), i32::MAX);
    }
}
