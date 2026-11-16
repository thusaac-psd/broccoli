use dashmap::{DashMap, DashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationRef {
    pub op_batch_id: String,
    pub op_task_id: String,
}

/// Minimum time a STARTED op is allowed to run before the result-wait gives up,
/// independent of the (small) solution-derived budget. Large in production so
/// system slowness (queueing, cold blob IO) yields slow verdicts instead of mass
/// timeouts; only a genuinely stuck op (dead worker / lost result) hits it, and
/// it is then re-dispatched. Zero in tests so the explicit `timeout` passed by
/// timeout-behavior tests still governs.
#[cfg(not(test))]
fn started_op_infra_floor() -> Duration {
    Duration::from_secs(30 * 60)
}
#[cfg(test)]
fn started_op_infra_floor() -> Duration {
    Duration::from_secs(0)
}

#[derive(Default)]
struct EvaluateBatchOps {
    by_test_case: DashMap<i32, Vec<OperationRef>>,
    by_operation_task: DashMap<String, i32>,
    started_at_by_test_case: DashMap<i32, Instant>,
    completed_test_cases: DashSet<i32>,
    cancelled_test_cases: DashSet<i32>,
    batch_cancelled: AtomicBool,
}

#[derive(Clone, Default)]
pub struct EvaluateBatchOpsRegistry {
    batches: Arc<DashMap<String, Arc<EvaluateBatchOps>>>,
}

impl EvaluateBatchOpsRegistry {
    pub fn record_ops(
        &self,
        evaluate_batch_id: &str,
        test_case_id: i32,
        op_batch_id: &str,
        op_task_ids: impl IntoIterator<Item = String>,
    ) {
        let batch = self
            .batches
            .entry(evaluate_batch_id.to_string())
            .or_insert_with(|| Arc::new(EvaluateBatchOps::default()))
            .clone();

        let refs = op_task_ids
            .into_iter()
            .map(|op_task_id| OperationRef {
                op_batch_id: op_batch_id.to_string(),
                op_task_id,
            })
            .collect::<Vec<_>>();

        batch
            .by_test_case
            .entry(test_case_id)
            .or_default()
            .extend(refs.clone());

        for op in refs {
            batch.by_operation_task.insert(op.op_task_id, test_case_id);
        }
    }

    pub fn remove_operation_batch(
        &self,
        evaluate_batch_id: &str,
        test_case_id: i32,
        op_batch_id: &str,
    ) {
        if let Some(batch) = self.batches.get(evaluate_batch_id)
            && let Some(mut refs) = batch.by_test_case.get_mut(&test_case_id)
        {
            let removed = refs
                .iter()
                .filter(|op| op.op_batch_id == op_batch_id)
                .map(|op| op.op_task_id.clone())
                .collect::<Vec<_>>();
            refs.retain(|op| op.op_batch_id != op_batch_id);
            for op_task_id in removed {
                batch.by_operation_task.remove(&op_task_id);
            }
        }
    }

    /// Record that an operation started, at the arrival of its start reply.
    pub fn mark_operation_started(&self, op_task_id: &str) -> bool {
        for batch in self.batches.iter() {
            if let Some(test_case_id) = batch.by_operation_task.get(op_task_id).map(|tc| *tc) {
                batch
                    .started_at_by_test_case
                    .entry(test_case_id)
                    .or_insert_with(Instant::now);
                return true;
            }
        }
        false
    }

    pub fn mark_test_case_completed(&self, evaluate_batch_id: &str, test_case_id: i32) {
        if let Some(batch) = self.batches.get(evaluate_batch_id) {
            batch.completed_test_cases.insert(test_case_id);
        }
    }

    pub fn should_extend_wait_for_execution_timeout(
        &self,
        evaluate_batch_id: &str,
        timeout: Duration,
    ) -> bool {
        let Some(batch) = self.batches.get(evaluate_batch_id) else {
            // Unknown batch -> EXTEND, not give up. A batch may legitimately be
            // unrecorded here: a just-re-dispatched op (new op-batch id not yet
            // recorded via record_ops), or a wait whose ops are tracked under a
            // different key. Returning false would surface a spurious timeout at
            // the very first tick before any real result can arrive. Extending is
            // bounded: the inner operation-result wait always produces a real or
            // timeout verdict within its own infra floor, which is then delivered
            // here, so this never hangs indefinitely.
            return true;
        };
        let now = Instant::now();
        // A STARTED op extends up to a large INFRASTRUCTURE ceiling, NOT the
        // small solution-derived `timeout`. Rationale: the solution's real time
        // limit is enforced inside isolate (it kills an overrunning solution and
        // returns a verdict, so a result always comes back for a live worker);
        // a genuinely DEAD worker is reclaimed by the dispatcher lease/steal
        // (new judge_epoch + re-dispatch). So the result-wait must not fail a
        // submission just because the SYSTEM is slow (deep queue, cold 30MB blob
        // fetches, IO thrash). Under load that would mass-timeout every
        // submission instead of degrading to merely-slow verdicts. The ceiling
        // only bounds a genuinely stuck op (dead worker / lost result), which
        // the windowed driver then re-dispatches. In tests the floor is 0 so the
        // passed `timeout` still governs (preserving timeout-behavior tests).
        let started_ceiling = timeout.max(started_op_infra_floor());

        batch.by_test_case.iter().any(|entry| {
            let test_case_id = *entry.key();
            if batch.completed_test_cases.contains(&test_case_id)
                || batch.cancelled_test_cases.contains(&test_case_id)
                || batch.batch_cancelled.load(Ordering::Acquire)
            {
                return false;
            }

            match batch.started_at_by_test_case.get(&test_case_id) {
                Some(started_at) => now.duration_since(*started_at) < started_ceiling,
                None => true,
            }
        })
    }

    /// Marks the given test cases as cancelled within an existing batch and
    /// returns how many were newly marked. If the batch has never recorded any
    /// operations (or has been removed), this is a no-op - we do not phantom-
    /// create a batch entry just to hold cancellation state, since the batch
    /// is gone and the entry would leak indefinitely.
    pub fn mark_cancelled(&self, evaluate_batch_id: &str, test_case_ids: &[i32]) -> usize {
        let Some(batch) = self.batches.get(evaluate_batch_id) else {
            return 0;
        };
        let batch = batch.clone();

        let mut newly_marked = 0usize;
        for test_case_id in test_case_ids {
            if batch.cancelled_test_cases.insert(*test_case_id) {
                newly_marked += 1;
            }
        }
        newly_marked
    }

    /// Marks the whole batch as cancelled. No-op if the batch is unknown,
    /// preventing phantom entries for already-removed batches.
    pub fn mark_batch_cancelled(&self, evaluate_batch_id: &str) {
        if let Some(batch) = self.batches.get(evaluate_batch_id) {
            batch.batch_cancelled.store(true, Ordering::Release);
        }
    }

    pub fn is_cancelled(&self, evaluate_batch_id: &str, test_case_id: i32) -> bool {
        self.batches.get(evaluate_batch_id).is_some_and(|batch| {
            batch.batch_cancelled.load(Ordering::Acquire)
                || batch.cancelled_test_cases.contains(&test_case_id)
        })
    }

    pub fn operation_task_ids_for_test_cases(
        &self,
        evaluate_batch_id: &str,
        test_case_ids: &[i32],
    ) -> Vec<String> {
        let Some(batch) = self.batches.get(evaluate_batch_id) else {
            return Vec::new();
        };

        test_case_ids
            .iter()
            .filter_map(|test_case_id| batch.by_test_case.get(test_case_id))
            .flat_map(|ops| {
                ops.iter()
                    .map(|op| op.op_task_id.clone())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub fn operation_task_ids_for_batch(&self, evaluate_batch_id: &str) -> Vec<String> {
        let Some(batch) = self.batches.get(evaluate_batch_id) else {
            return Vec::new();
        };

        batch
            .by_test_case
            .iter()
            .flat_map(|entry| {
                entry
                    .value()
                    .iter()
                    .map(|op| op.op_task_id.clone())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub fn remove_batch(&self, evaluate_batch_id: &str) {
        self.batches.remove(evaluate_batch_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_finds_operation_tasks_by_test_case() {
        let registry = EvaluateBatchOpsRegistry::default();
        registry.record_ops(
            "eval-1",
            10,
            "op-batch-1",
            ["op-1".to_string(), "op-2".to_string()],
        );
        registry.record_ops("eval-1", 11, "op-batch-2", ["op-3".to_string()]);

        assert_eq!(
            registry.operation_task_ids_for_test_cases("eval-1", &[10]),
            vec!["op-1".to_string(), "op-2".to_string()]
        );

        let mut all = registry.operation_task_ids_for_batch("eval-1");
        all.sort();
        assert_eq!(all, vec!["op-1", "op-2", "op-3"]);
    }

    #[test]
    fn marks_cancelled_cases_idempotently_for_existing_batch() {
        let registry = EvaluateBatchOpsRegistry::default();
        registry.record_ops("eval-1", 10, "op-batch-1", ["op-1".to_string()]);

        assert_eq!(registry.mark_cancelled("eval-1", &[10, 11]), 2);
        assert_eq!(registry.mark_cancelled("eval-1", &[10]), 0);
        assert!(registry.is_cancelled("eval-1", 10));
        assert!(registry.is_cancelled("eval-1", 11));
        assert!(!registry.is_cancelled("eval-1", 12));
    }

    #[test]
    fn mark_cancelled_does_not_phantom_create_unknown_batch() {
        let registry = EvaluateBatchOpsRegistry::default();
        assert_eq!(registry.mark_cancelled("never-recorded", &[10, 11]), 0);
        assert!(!registry.is_cancelled("never-recorded", 10));
        // No record left behind, so a later read on the same id stays empty.
        assert!(
            registry
                .operation_task_ids_for_batch("never-recorded")
                .is_empty()
        );
    }

    #[test]
    fn marks_entire_batch_cancelled() {
        let registry = EvaluateBatchOpsRegistry::default();
        registry.record_ops("eval-1", 10, "op-batch-1", ["op-1".to_string()]);

        registry.mark_batch_cancelled("eval-1");

        assert!(registry.is_cancelled("eval-1", 10));
        assert!(registry.is_cancelled("eval-1", 11));
    }

    #[test]
    fn mark_batch_cancelled_does_not_phantom_create_unknown_batch() {
        let registry = EvaluateBatchOpsRegistry::default();
        registry.mark_batch_cancelled("never-recorded");
        assert!(!registry.is_cancelled("never-recorded", 10));
        assert!(
            registry
                .operation_task_ids_for_batch("never-recorded")
                .is_empty()
        );
    }

    #[test]
    fn removes_operation_batch_refs() {
        let registry = EvaluateBatchOpsRegistry::default();
        registry.record_ops(
            "eval-1",
            10,
            "op-batch-1",
            ["op-1".to_string(), "op-2".to_string()],
        );
        registry.record_ops("eval-1", 10, "op-batch-2", ["op-3".to_string()]);

        registry.remove_operation_batch("eval-1", 10, "op-batch-1");

        assert_eq!(
            registry.operation_task_ids_for_test_cases("eval-1", &[10]),
            vec!["op-3".to_string()]
        );
    }

    #[test]
    fn started_at_uses_first_started_operation_for_test_case() {
        let registry = EvaluateBatchOpsRegistry::default();
        registry.record_ops(
            "eval-1",
            10,
            "op-batch-1",
            ["op-1".to_string(), "op-2".to_string()],
        );

        assert!(registry.mark_operation_started("op-2"));
        std::thread::sleep(Duration::from_millis(20));
        assert!(registry.mark_operation_started("op-1"));

        assert!(
            !registry.should_extend_wait_for_execution_timeout("eval-1", Duration::from_millis(10))
        );
    }

    #[test]
    fn queued_operation_extends_wait() {
        let registry = EvaluateBatchOpsRegistry::default();
        registry.record_ops("eval-1", 10, "op-batch-1", ["op-1".to_string()]);

        assert!(
            registry.should_extend_wait_for_execution_timeout("eval-1", Duration::from_millis(1))
        );
    }

    #[test]
    fn completed_test_case_does_not_extend_wait() {
        let registry = EvaluateBatchOpsRegistry::default();
        registry.record_ops("eval-1", 10, "op-batch-1", ["op-1".to_string()]);
        registry.mark_test_case_completed("eval-1", 10);

        assert!(
            !registry.should_extend_wait_for_execution_timeout("eval-1", Duration::from_secs(60))
        );
    }

    #[test]
    fn remove_batch_drops_refs_and_cancel_state() {
        let registry = EvaluateBatchOpsRegistry::default();
        registry.record_ops("eval-1", 10, "op-batch-1", ["op-1".to_string()]);
        registry.mark_cancelled("eval-1", &[10]);
        registry.mark_batch_cancelled("eval-1");

        registry.remove_batch("eval-1");

        assert!(registry.operation_task_ids_for_batch("eval-1").is_empty());
        assert!(!registry.is_cancelled("eval-1", 10));
    }
}
