use serde::{Deserialize, Serialize};

use crate::config::{TokenConfig, TokenMode};

/// A contestant's spent tokens: one per tokened submission.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenState {
    #[serde(default)]
    pub tokened_submission_ids: Vec<i32>,
}

impl TokenState {
    pub fn used(&self) -> u32 {
        self.tokened_submission_ids.len() as u32
    }
}

/// Compute the number of tokens currently available.
///
/// - `TokenMode::None` -> `u32::MAX` (tokens disabled, unlimited)
/// - `FixedBudget` -> `initial - used`
/// - `Regenerating` -> `min(initial + elapsed/interval, max) - used`
pub fn available_tokens(config: &TokenConfig, state: &TokenState, elapsed_minutes: u64) -> u32 {
    match config.mode {
        TokenMode::None => u32::MAX,
        TokenMode::FixedBudget => config.initial.saturating_sub(state.used()),
        TokenMode::Regenerating => {
            let regen_interval = config.regen_interval_min.max(1) as u64;
            let regenerated = elapsed_minutes / regen_interval;
            let total = (config.initial as u64 + regenerated).min(config.max as u64) as u32;
            total.saturating_sub(state.used())
        }
    }
}

/// Compute the next regeneration boundary, in elapsed minutes since contest start.
pub fn next_regen_elapsed_min(config: &TokenConfig, elapsed_minutes: u64) -> Option<u64> {
    match config.mode {
        TokenMode::None | TokenMode::FixedBudget => None,
        TokenMode::Regenerating => {
            let regen_interval = config.regen_interval_min.max(1) as u64;
            let regenerated = elapsed_minutes / regen_interval;
            let current_total = (config.initial as u64 + regenerated).min(config.max as u64);
            if current_total >= config.max as u64 {
                None
            } else {
                Some((regenerated + 1) * regen_interval)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_config(initial: u32) -> TokenConfig {
        TokenConfig {
            mode: TokenMode::FixedBudget,
            initial,
            max: initial,
            regen_interval_min: 30,
        }
    }

    fn spent(n: i32) -> TokenState {
        TokenState {
            tokened_submission_ids: (1..=n).collect(),
        }
    }

    fn regen_config(initial: u32, max: u32, interval: u32) -> TokenConfig {
        TokenConfig {
            mode: TokenMode::Regenerating,
            initial,
            max,
            regen_interval_min: interval,
        }
    }

    #[test]
    fn none_unlimited() {
        let config = TokenConfig {
            mode: TokenMode::None,
            ..Default::default()
        };
        let state = TokenState::default();
        assert_eq!(available_tokens(&config, &state, 0), u32::MAX);
    }

    #[test]
    fn fixed_initial() {
        let config = fixed_config(5);
        let state = spent(2);
        assert_eq!(available_tokens(&config, &state, 0), 3);
    }

    #[test]
    fn fixed_exhausted() {
        let config = fixed_config(3);
        let state = spent(3);
        assert_eq!(available_tokens(&config, &state, 0), 0);
    }

    #[test]
    fn regen_initial() {
        let config = regen_config(2, 10, 30);
        let state = TokenState::default();
        assert_eq!(available_tokens(&config, &state, 0), 2);
    }

    #[test]
    fn regen_after_interval() {
        let config = regen_config(2, 10, 30);
        let state = TokenState::default();
        // After 60 minutes: 2 + 60/30 = 4
        assert_eq!(available_tokens(&config, &state, 60), 4);
    }

    #[test]
    fn regen_capped() {
        let config = regen_config(2, 5, 30);
        let state = TokenState::default();
        // After 300 min: 2 + 10 = 12, capped at 5
        assert_eq!(available_tokens(&config, &state, 300), 5);
    }

    #[test]
    fn regen_with_used() {
        let config = regen_config(2, 10, 30);
        let state = spent(3);
        // After 60 min: min(2 + 2, 10) - 3 = 1
        assert_eq!(available_tokens(&config, &state, 60), 1);
    }

    #[test]
    fn next_regen_none_mode() {
        let config = TokenConfig {
            mode: TokenMode::None,
            ..Default::default()
        };
        assert_eq!(next_regen_elapsed_min(&config, 0), None);
    }

    #[test]
    fn next_regen_fixed_budget_mode() {
        let config = fixed_config(5);
        assert_eq!(next_regen_elapsed_min(&config, 15), None);
    }

    #[test]
    fn next_regen_initial_boundary() {
        let config = regen_config(2, 10, 30);
        assert_eq!(next_regen_elapsed_min(&config, 0), Some(30));
    }

    #[test]
    fn next_regen_between_boundaries() {
        let config = regen_config(2, 10, 30);
        assert_eq!(next_regen_elapsed_min(&config, 44), Some(60));
    }

    #[test]
    fn next_regen_after_multiple_intervals() {
        let config = regen_config(2, 10, 30);
        assert_eq!(next_regen_elapsed_min(&config, 60), Some(90));
    }

    #[test]
    fn next_regen_none_when_schedule_is_capped() {
        let config = regen_config(2, 5, 30);
        assert_eq!(next_regen_elapsed_min(&config, 90), None);
    }
}
