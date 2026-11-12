//! Per-test-case progress of one evaluate batch, kept as the batch's state in
//! its [`BatchTable`](crate::services::batch_table::BatchTable) entry so it is
//! created and removed together with the batch.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::registry::EvaluateBatches;

/// Minimum time a STARTED case is allowed to run before the result wait gives
/// up, independent of the (small) solution-derived budget. Large in
/// production: the solution's real time limit is enforced inside the sandbox,
/// which always returns a verdict for a live worker, and a dead worker is
/// reclaimed by the dispatcher lease/steal. So system slowness (deep queues,
/// cold blob fetches) yields slow verdicts instead of mass timeouts, and only
/// a genuinely stuck case (dead worker, lost result) reaches this ceiling,
/// after which the windowed driver re-dispatches it. Zero in tests so the
/// explicit `timeout` still governs.
#[cfg(not(test))]
fn started_case_infra_floor() -> Duration {
    Duration::from_secs(30 * 60)
}
#[cfg(test)]
fn started_case_infra_floor() -> Duration {
    Duration::ZERO
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OperationRef {
    op_batch_id: String,
    op_task_id: String,
}

/// Where one test case of an evaluate batch is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaseProgress {
    /// Being prepared, or its operations are queued on a worker.
    Waiting,
    /// The first of its operations started executing at this instant.
    Started(Instant),
    /// Its verdict was delivered.
    Completed,
    /// The plugin cancelled it before a verdict was delivered.
    Cancelled,
}

#[derive(Debug)]
struct Case {
    progress: CaseProgress,
    ops: Vec<OperationRef>,
}

/// The test cases of one evaluate batch and the operations started for them.
#[derive(Debug, Default)]
pub struct EvaluateCases {
    cases: HashMap<i32, Case>,
}

impl EvaluateCases {
    pub fn new(test_case_ids: impl IntoIterator<Item = i32>) -> Self {
        Self {
            cases: test_case_ids
                .into_iter()
                .map(|id| {
                    (
                        id,
                        Case {
                            progress: CaseProgress::Waiting,
                            ops: Vec::new(),
                        },
                    )
                })
                .collect(),
        }
    }

    fn record_ops(&mut self, test_case_id: i32, op_batch_id: &str, op_task_ids: Vec<String>) {
        if let Some(case) = self.cases.get_mut(&test_case_id) {
            case.ops
                .extend(op_task_ids.into_iter().map(|op_task_id| OperationRef {
                    op_batch_id: op_batch_id.to_string(),
                    op_task_id,
                }));
        }
    }

    fn remove_operation_batch(&mut self, test_case_id: i32, op_batch_id: &str) {
        if let Some(case) = self.cases.get_mut(&test_case_id) {
            case.ops.retain(|op| op.op_batch_id != op_batch_id);
        }
    }

    fn mark_started(&mut self, test_case_id: i32, at: Instant) {
        if let Some(case) = self.cases.get_mut(&test_case_id)
            && case.progress == CaseProgress::Waiting
        {
            case.progress = CaseProgress::Started(at);
        }
    }

    fn mark_completed(&mut self, test_case_id: i32) {
        if let Some(case) = self.cases.get_mut(&test_case_id) {
            case.progress = CaseProgress::Completed;
        }
    }

    /// Cancel the given cases that have no verdict yet; returns how many
    /// changed.
    fn cancel(&mut self, test_case_ids: &[i32]) -> usize {
        let mut cancelled = 0;
        for id in test_case_ids {
            if let Some(case) = self.cases.get_mut(id)
                && matches!(
                    case.progress,
                    CaseProgress::Waiting | CaseProgress::Started(_)
                )
            {
                case.progress = CaseProgress::Cancelled;
                cancelled += 1;
            }
        }
        cancelled
    }

    /// Whether a result wait on this batch that just saw an empty tick keeps
    /// waiting at `now`: some case has no verdict and is either still waiting
    /// for a worker, or started less than its execution ceiling ago.
    fn should_extend(&self, timeout: Duration, now: Instant) -> bool {
        let started_ceiling = timeout.max(started_case_infra_floor());
        self.cases.values().any(|case| match case.progress {
            CaseProgress::Waiting => true,
            CaseProgress::Started(at) => now.saturating_duration_since(at) < started_ceiling,
            CaseProgress::Completed | CaseProgress::Cancelled => false,
        })
    }
}

fn op_task_ids<'a>(cases: impl Iterator<Item = &'a Case>) -> Vec<String> {
    cases
        .flat_map(|case| case.ops.iter().map(|op| op.op_task_id.clone()))
        .collect()
}

/// Case-level operations on the evaluate batch table. Each is a no-op on a
/// batch that is no longer live.
impl EvaluateBatches {
    /// Record operations started for one of the batch's test cases.
    pub fn record_ops(
        &self,
        batch_id: &str,
        test_case_id: i32,
        op_batch_id: &str,
        op_task_ids: Vec<String>,
    ) {
        self.update(batch_id, |cases| {
            cases.record_ops(test_case_id, op_batch_id, op_task_ids)
        });
    }

    /// Forget the operations of an operation batch that failed to start.
    pub fn remove_operation_batch(&self, batch_id: &str, test_case_id: i32, op_batch_id: &str) {
        self.update(batch_id, |cases| {
            cases.remove_operation_batch(test_case_id, op_batch_id)
        });
    }

    /// Note that one of a case's operations started executing at `at`.
    /// Returns whether the batch is live.
    pub fn mark_case_started(&self, batch_id: &str, test_case_id: i32, at: Instant) -> bool {
        self.update(batch_id, |cases| cases.mark_started(test_case_id, at))
            .is_some()
    }

    pub fn mark_case_completed(&self, batch_id: &str, test_case_id: i32) {
        self.update(batch_id, |cases| cases.mark_completed(test_case_id));
    }

    /// Cancel the given cases that have no verdict yet; returns how many
    /// changed.
    pub fn cancel_cases(&self, batch_id: &str, test_case_ids: &[i32]) -> usize {
        self.update(batch_id, |cases| cases.cancel(test_case_ids))
            .unwrap_or(0)
    }

    /// Task ids of the operations started for the given cases.
    pub fn operation_task_ids_for_cases(
        &self,
        batch_id: &str,
        test_case_ids: &[i32],
    ) -> Vec<String> {
        self.read(batch_id, |cases| {
            op_task_ids(test_case_ids.iter().filter_map(|id| cases.cases.get(id)))
        })
        .unwrap_or_default()
    }

    /// Task ids of every operation started for the batch.
    pub fn operation_task_ids(&self, batch_id: &str) -> Vec<String> {
        self.read(batch_id, |cases| op_task_ids(cases.cases.values()))
            .unwrap_or_default()
    }

    /// See [`EvaluateCases::should_extend`]. A batch that is no longer live
    /// extends: its removal disconnects the wait.
    pub(crate) fn should_extend_wait(
        &self,
        batch_id: &str,
        timeout: Duration,
        now: Instant,
    ) -> bool {
        self.read(batch_id, |cases| cases.should_extend(timeout, now))
            .unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::batch_table::Removal;

    fn batch(ids: &[i32]) -> EvaluateBatches {
        let batches = EvaluateBatches::default();
        batches.open("eval-1", ids.len(), EvaluateCases::new(ids.iter().copied()));
        batches
    }

    #[test]
    fn finds_operation_tasks_by_test_case() {
        let batches = batch(&[10, 11]);
        batches.record_ops("eval-1", 10, "op-1", vec!["a".into(), "b".into()]);
        batches.record_ops("eval-1", 11, "op-2", vec!["c".into()]);

        assert_eq!(
            batches.operation_task_ids_for_cases("eval-1", &[10]),
            vec!["a", "b"]
        );
        let mut all = batches.operation_task_ids("eval-1");
        all.sort();
        assert_eq!(all, vec!["a", "b", "c"]);
    }

    #[test]
    fn removed_operation_batch_drops_its_refs() {
        let batches = batch(&[10]);
        batches.record_ops("eval-1", 10, "op-1", vec!["a".into()]);
        batches.record_ops("eval-1", 10, "op-2", vec!["b".into()]);
        batches.remove_operation_batch("eval-1", 10, "op-1");
        assert_eq!(batches.operation_task_ids("eval-1"), vec!["b"]);
    }

    #[test]
    fn operations_for_a_removed_batch_are_not_recorded() {
        let batches = batch(&[10]);
        batches.remove("eval-1", Removal::Superseded);
        batches.record_ops("eval-1", 10, "op-1", vec!["a".into()]);
        assert!(!batches.contains("eval-1"));
        assert!(batches.operation_task_ids("eval-1").is_empty());
    }

    #[test]
    fn cancel_counts_only_cases_without_a_verdict() {
        let batches = batch(&[10, 11, 12]);
        batches.mark_case_completed("eval-1", 12);
        assert_eq!(batches.cancel_cases("eval-1", &[10, 11, 12, 99]), 2);
        assert_eq!(batches.cancel_cases("eval-1", &[10]), 0);
        assert_eq!(batches.cancel_cases("missing", &[10]), 0);
    }

    #[test]
    fn waiting_case_extends_the_wait() {
        let batches = batch(&[10]);
        assert!(batches.should_extend_wait("eval-1", Duration::from_millis(1), Instant::now()));
    }

    #[test]
    fn started_case_extends_until_its_ceiling_from_the_first_start() {
        let batches = batch(&[10]);
        let t0 = Instant::now();
        let timeout = Duration::from_millis(100);
        batches.mark_case_started("eval-1", 10, t0);
        batches.mark_case_started("eval-1", 10, t0 + Duration::from_millis(80));

        assert!(batches.should_extend_wait("eval-1", timeout, t0 + Duration::from_millis(99)));
        assert!(!batches.should_extend_wait("eval-1", timeout, t0 + timeout));
    }

    #[test]
    fn finished_cases_do_not_extend_the_wait() {
        let batches = batch(&[10, 11]);
        batches.mark_case_completed("eval-1", 10);
        batches.cancel_cases("eval-1", &[11]);
        assert!(!batches.should_extend_wait("eval-1", Duration::from_secs(60), Instant::now()));
    }

    #[test]
    fn a_case_cannot_be_both_completed_and_cancelled() {
        let batches = batch(&[10]);
        batches.cancel_cases("eval-1", &[10]);
        batches.mark_case_started("eval-1", 10, Instant::now());
        assert_eq!(batches.cancel_cases("eval-1", &[10]), 0);
        batches.mark_case_completed("eval-1", 10);
        assert_eq!(batches.cancel_cases("eval-1", &[10]), 0);
    }
}
