use crate::config::{round_score, total_score};

/// MaxSubmission mode: task score = max(current submission score, historical best).
pub fn score_max_submission(current: f64, historical_best: f64) -> f64 {
    round_score(current.max(historical_best))
}

/// SumBestSubtask mode: for each subtask position, take the max score across
/// all submissions, then sum. Each inner `Vec<f64>` is one submission's subtask
/// scores (indexed by subtask position).
pub fn score_sum_best_subtask(all_submissions_subtask_scores: &[Vec<f64>]) -> f64 {
    if all_submissions_subtask_scores.is_empty() {
        return 0.0;
    }

    let num_subtasks = all_submissions_subtask_scores
        .iter()
        .map(|s| s.len())
        .max()
        .unwrap_or(0);

    let mut best_per_subtask = vec![0.0_f64; num_subtasks];
    for submission_scores in all_submissions_subtask_scores {
        for (i, &score) in submission_scores.iter().enumerate() {
            if score > best_per_subtask[i] {
                best_per_subtask[i] = score;
            }
        }
    }

    round_score(total_score(best_per_subtask))
}

/// BestTokenedOrLast mode: task score = max(best tokened submission score, last submission score).
pub fn score_best_tokened_or_last(tokened_best: f64, last_score: f64) -> f64 {
    round_score(tokened_best.max(last_score))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_new_higher() {
        assert_eq!(score_max_submission(80.0, 50.0), 80.0);
    }

    #[test]
    fn max_historical_higher() {
        assert_eq!(score_max_submission(30.0, 70.0), 70.0);
    }

    #[test]
    fn max_equal() {
        assert_eq!(score_max_submission(50.0, 50.0), 50.0);
    }

    #[test]
    fn sum_best_first_submission() {
        let scores = vec![vec![10.0, 20.0, 30.0]];
        assert_eq!(score_sum_best_subtask(&scores), 60.0);
    }

    #[test]
    fn sum_best_improves_one() {
        let scores = vec![
            vec![10.0, 20.0, 30.0], // sub 1
            vec![10.0, 50.0, 10.0], // sub 2: subtask 1 same, subtask 2 better, subtask 3 worse
        ];
        // best per subtask: [10, 50, 30] = 90
        assert_eq!(score_sum_best_subtask(&scores), 90.0);
    }

    #[test]
    fn sum_best_no_submissions() {
        assert_eq!(score_sum_best_subtask(&[]), 0.0);
    }

    #[test]
    fn sum_best_no_improvement() {
        let scores = vec![
            vec![30.0, 30.0],
            vec![20.0, 20.0], // worse on both
        ];
        assert_eq!(score_sum_best_subtask(&scores), 60.0);
    }

    #[test]
    fn tokened_better() {
        assert_eq!(score_best_tokened_or_last(80.0, 50.0), 80.0);
    }

    #[test]
    fn last_better() {
        assert_eq!(score_best_tokened_or_last(30.0, 70.0), 70.0);
    }

    #[test]
    fn no_tokened_subs() {
        // If no tokened submissions, tokened_best is 0
        assert_eq!(score_best_tokened_or_last(0.0, 45.0), 45.0);
    }
}
