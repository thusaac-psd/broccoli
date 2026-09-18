#[cfg(not(target_arch = "wasm32"))]
use std::cell::RefCell;
#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;
use std::collections::{HashSet, VecDeque};
use std::time::Instant;

use crate::error::SdkError;
#[cfg(target_arch = "wasm32")]
use crate::types::DetachedEvaluateSession;
use crate::types::{
    DEFAULT_EVALUATION_RESULT_TIMEOUT_MAX_MS, DetachedSubmissionCompletion, OnSubmissionInput,
    StartDetachedWindowedEvaluateInput, StartEvaluateBatchInput, StartEvaluateCaseInput,
    TestCaseVerdict, default_evaluation_result_timeout_ms,
};

const WINDOWED_POLL_SLICE_MS: u64 = 50;

pub struct Eval {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) inner: EvalMock,
}

pub struct WindowedEvalSession<'a> {
    eval: &'a Eval,
    problem_type: String,
    window_size: usize,
    pending: VecDeque<StartEvaluateCaseInput>,
    active: Vec<ActiveEvalBatch>,
    wait_cursor: usize,
    deferred_refill_error: Option<SdkError>,
}

pub struct WindowedEvalBuilder<'a> {
    eval: &'a Eval,
    input: StartEvaluateBatchInput,
    concurrency: usize,
    result_timeout_ms: u64,
    submission_completion: Option<DetachedSubmissionCompletion>,
}

struct ActiveEvalBatch {
    batch_id: String,
    test_case_ids: Vec<i32>,
}

impl Eval {
    pub fn windowed<'a>(&'a self, input: &StartEvaluateBatchInput) -> WindowedEvalBuilder<'a> {
        let result_timeout_ms = input
            .test_cases
            .iter()
            .map(|case| default_evaluation_result_timeout_ms(case.time_limit_ms))
            .max()
            .unwrap_or(DEFAULT_EVALUATION_RESULT_TIMEOUT_MAX_MS);
        WindowedEvalBuilder {
            eval: self,
            input: input.clone(),
            concurrency: 1,
            result_timeout_ms,
            submission_completion: None,
        }
    }
}

impl<'a> WindowedEvalBuilder<'a> {
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    pub fn result_timeout_ms(mut self, result_timeout_ms: u64) -> Self {
        self.result_timeout_ms = result_timeout_ms;
        self
    }

    pub fn fire_after_judging_for_submission(mut self, req: &OnSubmissionInput) -> Self {
        self.submission_completion = Some(DetachedSubmissionCompletion {
            submission_id: req.submission_id,
            judgement_id: req.judgement_id,
            judge_epoch: req.judge_epoch,
            fire_after_judging: req.fire_after_judging,
        });
        self
    }

    pub fn start(self) -> Result<WindowedEvalSession<'a>, SdkError> {
        let mut session = WindowedEvalSession {
            eval: self.eval,
            problem_type: self.input.problem_type,
            window_size: self.concurrency,
            pending: self.input.test_cases.into(),
            active: Vec::new(),
            wait_cursor: 0,
            deferred_refill_error: None,
        };
        session.start_initial_batch()?;
        Ok(session)
    }

    pub fn start_detached(
        self,
        callback_fn: impl Into<String>,
        state: serde_json::Value,
    ) -> Result<String, SdkError> {
        let input = StartDetachedWindowedEvaluateInput {
            batch: self.input,
            concurrency: self.concurrency,
            result_timeout_ms: self.result_timeout_ms,
            callback_fn: callback_fn.into(),
            state,
            submission_completion: self.submission_completion,
        };
        self.eval.start_detached_windowed(&input)
    }
}

impl WindowedEvalSession<'_> {
    pub fn next_result(&mut self, timeout_ms: u64) -> Result<Option<TestCaseVerdict>, SdkError> {
        self.next_result_with_refill_filter(timeout_ms, |_| true)
    }

    pub fn next_result_with_refill_filter(
        &mut self,
        timeout_ms: u64,
        mut should_refill: impl FnMut(&TestCaseVerdict) -> bool,
    ) -> Result<Option<TestCaseVerdict>, SdkError> {
        let start = Instant::now();

        loop {
            if self.active.is_empty() {
                if let Some(err) = self.deferred_refill_error.take() {
                    return Err(err);
                }
                self.refill_window()?;
            }

            if self.active.is_empty() {
                return Ok(None);
            }

            if let Some(result) = self.poll_active_batches_once(0, &mut should_refill)? {
                return Ok(Some(result));
            }

            if let Some(err) = self.deferred_refill_error.take() {
                return Err(err);
            }

            if self.active_test_case_count() < self.window_size {
                self.refill_window()?;
            }

            if timeout_ms == 0 {
                return Ok(None);
            }

            let elapsed_ms = start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            if elapsed_ms >= timeout_ms {
                return Ok(None);
            }

            let wait_ms = (timeout_ms - elapsed_ms).min(WINDOWED_POLL_SLICE_MS);
            let batch_id = self.next_batch_id_to_wait_on();
            if let Some(result) = self.eval.next_result(&batch_id, wait_ms)? {
                let refill_after_result = should_refill(&result);
                if let Some(result) = self.record_result(result, refill_after_result)? {
                    return Ok(Some(result));
                }
            }
        }
    }

    pub fn cancel_all(&mut self) -> Result<(), SdkError> {
        self.pending.clear();
        self.deferred_refill_error = None;
        let active = self
            .active
            .iter()
            .map(|batch| batch.batch_id.clone())
            .collect::<Vec<_>>();
        let mut cancelled = HashSet::new();
        let mut first_error = None;

        for batch_id in active {
            if let Err(err) = self.eval.cancel_batch(&batch_id) {
                if first_error.is_none() {
                    first_error = Some(err);
                }
            } else {
                cancelled.insert(batch_id);
            }
        }

        self.active
            .retain(|batch| !cancelled.contains(&batch.batch_id));

        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    pub fn cancel_test_cases(&mut self, test_case_ids: &[i32]) -> Result<usize, SdkError> {
        let test_case_ids: HashSet<i32> = test_case_ids.iter().copied().collect();
        let pending_before = self.pending.len();
        let active_cancels = self
            .active
            .iter()
            .filter_map(|batch| {
                let ids = batch
                    .test_case_ids
                    .iter()
                    .copied()
                    .filter(|id| test_case_ids.contains(id))
                    .collect::<Vec<_>>();
                (!ids.is_empty()).then_some((batch.batch_id.clone(), ids))
            })
            .collect::<Vec<_>>();

        let mut cancel_results = Vec::with_capacity(active_cancels.len());
        let mut batches_to_cancel = Vec::new();
        let mut first_error: Option<SdkError> = None;
        for (batch_id, ids) in &active_cancels {
            let count = match self.eval.cancel_test_cases(batch_id, ids) {
                Ok(count) => count,
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                    continue;
                }
            };
            if count != ids.len() {
                if first_error.is_none() {
                    first_error = Some(SdkError::HostCall(format!(
                        "cancel_test_cases cancelled {count}/{} requested test cases for batch {batch_id}",
                        ids.len()
                    )));
                }
                continue;
            }
            if self
                .active
                .iter()
                .find(|batch| batch.batch_id.as_str() == batch_id.as_str())
                .is_some_and(|batch| batch.test_case_ids.len() == ids.len())
            {
                batches_to_cancel.push(batch_id.clone());
            }
            cancel_results.push((batch_id.clone(), ids.clone(), count));
        }

        let mut cancelled = 0usize;
        for (batch_id, ids, count) in cancel_results {
            cancelled += count;
            let cancelled_ids = ids.into_iter().collect::<HashSet<_>>();
            if let Some(batch) = self
                .active
                .iter_mut()
                .find(|batch| batch.batch_id == batch_id)
            {
                batch.test_case_ids.retain(|id| !cancelled_ids.contains(id));
            }
        }

        self.active.retain(|batch| !batch.test_case_ids.is_empty());
        for batch_id in &batches_to_cancel {
            let _ = self.eval.cancel_batch(batch_id);
        }

        if first_error.is_none() {
            self.pending
                .retain(|case| !test_case_ids.contains(&case.test_case_id));
            let dropped_pending = pending_before - self.pending.len();
            cancelled += dropped_pending;

            if let Err(err) = self.refill_window() {
                self.deferred_refill_error.get_or_insert(err);
            }
        }

        match first_error {
            Some(err) => Err(err),
            None => Ok(cancelled),
        }
    }

    fn start_initial_batch(&mut self) -> Result<(), SdkError> {
        let count = self.window_size.min(self.pending.len());
        if count == 0 {
            return Ok(());
        }

        let test_cases = (0..count)
            .filter_map(|_| self.pending.pop_front())
            .collect();
        self.start_batch(test_cases)
    }

    fn poll_active_batches_once(
        &mut self,
        timeout_ms: u64,
        should_refill: &mut impl FnMut(&TestCaseVerdict) -> bool,
    ) -> Result<Option<TestCaseVerdict>, SdkError> {
        let batch_ids: Vec<String> = self
            .active
            .iter()
            .map(|batch| batch.batch_id.clone())
            .collect();

        for batch_id in batch_ids {
            if let Some(result) = self.eval.next_result(&batch_id, timeout_ms)? {
                let refill_after_result = should_refill(&result);
                if let Some(result) = self.record_result(result, refill_after_result)? {
                    return Ok(Some(result));
                }
            }
        }

        Ok(None)
    }

    fn record_result(
        &mut self,
        result: TestCaseVerdict,
        refill_after_result: bool,
    ) -> Result<Option<TestCaseVerdict>, SdkError> {
        if !self.remove_active_test_case(result.test_case_id) {
            return Ok(None);
        }
        if refill_after_result && let Err(err) = self.refill_window() {
            self.deferred_refill_error.get_or_insert(err);
        }
        Ok(Some(result))
    }

    fn remove_active_test_case(&mut self, test_case_id: i32) -> bool {
        let mut removed = false;
        for batch in &mut self.active {
            if let Some(index) = batch
                .test_case_ids
                .iter()
                .position(|id| *id == test_case_id)
            {
                batch.test_case_ids.remove(index);
                removed = true;
                break;
            }
        }
        self.active.retain(|batch| !batch.test_case_ids.is_empty());
        removed
    }

    fn refill_window(&mut self) -> Result<(), SdkError> {
        while self.active_test_case_count() < self.window_size {
            let Some(test_case) = self.pending.front().cloned() else {
                break;
            };
            self.start_batch(vec![test_case])?;
            self.pending.pop_front();
        }
        self.deferred_refill_error = None;
        Ok(())
    }

    fn start_batch(&mut self, test_cases: Vec<StartEvaluateCaseInput>) -> Result<(), SdkError> {
        let test_case_ids = test_cases
            .iter()
            .map(|case| case.test_case_id)
            .collect::<Vec<_>>();
        let input = StartEvaluateBatchInput {
            problem_type: self.problem_type.clone(),
            test_cases,
        };
        let batch_id = self.eval.start_batch(&input)?;
        self.active.push(ActiveEvalBatch {
            batch_id,
            test_case_ids,
        });
        Ok(())
    }

    fn active_test_case_count(&self) -> usize {
        self.active
            .iter()
            .map(|batch| batch.test_case_ids.len())
            .sum()
    }

    fn next_batch_id_to_wait_on(&mut self) -> String {
        let index = self.wait_cursor % self.active.len();
        self.wait_cursor = (index + 1) % self.active.len();
        self.active[index].batch_id.clone()
    }
}

#[cfg(target_arch = "wasm32")]
impl Eval {
    fn start_detached_windowed(
        &self,
        input: &StartDetachedWindowedEvaluateInput,
    ) -> Result<String, SdkError> {
        let input_json = serde_json::to_string(input)?;
        let response_json =
            unsafe { crate::host::raw::start_detached_windowed_evaluate(input_json)? };
        let response: DetachedEvaluateSession = serde_json::from_str(&response_json)?;
        Ok(response.session_id)
    }

    fn start_batch(&self, input: &StartEvaluateBatchInput) -> Result<String, SdkError> {
        let input_json = serde_json::to_string(input)?;
        let response_json = unsafe { crate::host::raw::start_evaluate_batch(input_json)? };
        let response: serde_json::Value = serde_json::from_str(&response_json)?;
        response["batch_id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| {
                SdkError::HostCall("Missing batch_id in start_evaluate_batch response".into())
            })
    }

    fn next_result(
        &self,
        batch_id: &str,
        timeout_ms: u64,
    ) -> Result<Option<TestCaseVerdict>, SdkError> {
        let input = serde_json::json!({
            "batch_id": batch_id,
            "timeout_ms": timeout_ms,
        });
        let result_json =
            unsafe { crate::host::raw::get_next_evaluate_result(serde_json::to_string(&input)?)? };
        let envelope: serde_json::Value = serde_json::from_str(&result_json)?;

        match envelope.get("result").filter(|v| !v.is_null()) {
            Some(v) => Ok(Some(serde_json::from_value(v.clone())?)),
            None => Ok(None),
        }
    }

    fn cancel_batch(&self, batch_id: &str) -> Result<(), SdkError> {
        let input = serde_json::json!({ "batch_id": batch_id });
        unsafe { crate::host::raw::cancel_evaluate_batch(serde_json::to_string(&input)?)? };
        Ok(())
    }

    fn cancel_test_cases(&self, batch_id: &str, test_case_ids: &[i32]) -> Result<usize, SdkError> {
        let input = serde_json::json!({
            "evaluate_batch_id": batch_id,
            "test_case_ids": test_case_ids,
        });
        let response_json = unsafe {
            crate::host::raw::cancel_evaluate_test_cases(serde_json::to_string(&input)?)?
        };
        let response: serde_json::Value = serde_json::from_str(&response_json)?;
        response["count"]
            .as_u64()
            .map(|count| count as usize)
            .ok_or_else(|| {
                SdkError::HostCall("Missing count in cancel_evaluate_test_cases response".into())
            })
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) struct EvalMock {
    results: RefCell<VecDeque<TestCaseVerdict>>,
    batch_results: RefCell<HashMap<String, VecDeque<TestCaseVerdict>>>,
    start_errors: RefCell<VecDeque<SdkError>>,
    result_errors: RefCell<VecDeque<SdkError>>,
    batch_inputs: RefCell<Vec<StartEvaluateBatchInput>>,
    result_timeouts: RefCell<Vec<u64>>,
    cancels: RefCell<Vec<String>>,
    cancel_errors: RefCell<VecDeque<SdkError>>,
    testcase_cancels: RefCell<Vec<(String, Vec<i32>)>>,
    testcase_cancel_results: RefCell<VecDeque<Result<usize, SdkError>>>,
    detached_windowed_requests: RefCell<Vec<StartDetachedWindowedEvaluateInput>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl EvalMock {
    pub fn new() -> Self {
        Self {
            results: RefCell::new(VecDeque::new()),
            batch_results: RefCell::new(HashMap::new()),
            start_errors: RefCell::new(VecDeque::new()),
            result_errors: RefCell::new(VecDeque::new()),
            batch_inputs: RefCell::new(Vec::new()),
            result_timeouts: RefCell::new(Vec::new()),
            cancels: RefCell::new(Vec::new()),
            cancel_errors: RefCell::new(VecDeque::new()),
            testcase_cancels: RefCell::new(Vec::new()),
            testcase_cancel_results: RefCell::new(VecDeque::new()),
            detached_windowed_requests: RefCell::new(Vec::new()),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Eval {
    fn start_detached_windowed(
        &self,
        input: &StartDetachedWindowedEvaluateInput,
    ) -> Result<String, SdkError> {
        self.inner
            .detached_windowed_requests
            .borrow_mut()
            .push(input.clone());
        Ok(format!(
            "mock-detached-eval-{}",
            self.inner.detached_windowed_requests.borrow().len()
        ))
    }

    fn start_batch(&self, input: &StartEvaluateBatchInput) -> Result<String, SdkError> {
        if let Some(err) = self.inner.start_errors.borrow_mut().pop_front() {
            return Err(err);
        }
        self.inner.batch_inputs.borrow_mut().push(input.clone());
        Ok(format!(
            "mock-batch-{}",
            self.inner.batch_inputs.borrow().len()
        ))
    }

    fn next_result(
        &self,
        batch_id: &str,
        timeout_ms: u64,
    ) -> Result<Option<TestCaseVerdict>, SdkError> {
        self.inner.result_timeouts.borrow_mut().push(timeout_ms);
        if let Some(err) = self.inner.result_errors.borrow_mut().pop_front() {
            return Err(err);
        }
        let mut batch_results = self.inner.batch_results.borrow_mut();
        if !batch_results.is_empty() {
            let result = batch_results
                .get_mut(batch_id)
                .and_then(VecDeque::pop_front);
            batch_results.retain(|_, results| !results.is_empty());
            if result.is_some() || !batch_results.is_empty() {
                return Ok(result);
            }
        }
        drop(batch_results);
        Ok(self.inner.results.borrow_mut().pop_front())
    }

    fn cancel_batch(&self, batch_id: &str) -> Result<(), SdkError> {
        self.inner.cancels.borrow_mut().push(batch_id.to_string());
        if let Some(err) = self.inner.cancel_errors.borrow_mut().pop_front() {
            return Err(err);
        }
        Ok(())
    }

    fn cancel_test_cases(&self, batch_id: &str, test_case_ids: &[i32]) -> Result<usize, SdkError> {
        self.inner
            .testcase_cancels
            .borrow_mut()
            .push((batch_id.to_string(), test_case_ids.to_vec()));
        if let Some(result) = self.inner.testcase_cancel_results.borrow_mut().pop_front() {
            return result;
        }
        Ok(test_case_ids.len())
    }

    pub fn queue_result(&self, v: TestCaseVerdict) {
        self.inner.results.borrow_mut().push_back(v);
    }

    pub fn queue_result_for_batch(&self, batch_id: &str, v: TestCaseVerdict) {
        self.inner
            .batch_results
            .borrow_mut()
            .entry(batch_id.to_string())
            .or_default()
            .push_back(v);
    }

    pub fn queue_start_error(&self, err: SdkError) {
        self.inner.start_errors.borrow_mut().push_back(err);
    }

    pub fn queue_result_error(&self, err: SdkError) {
        self.inner.result_errors.borrow_mut().push_back(err);
    }

    pub fn queue_cancel_error(&self, err: SdkError) {
        self.inner.cancel_errors.borrow_mut().push_back(err);
    }

    pub fn queue_testcase_cancel_result(&self, result: Result<usize, SdkError>) {
        self.inner
            .testcase_cancel_results
            .borrow_mut()
            .push_back(result);
    }

    pub fn batch_inputs(&self) -> Vec<StartEvaluateBatchInput> {
        self.inner.batch_inputs.borrow().clone()
    }

    pub fn result_timeouts(&self) -> Vec<u64> {
        self.inner.result_timeouts.borrow().clone()
    }

    pub fn was_cancelled(&self) -> bool {
        !self.inner.cancels.borrow().is_empty()
    }

    pub fn testcase_cancels(&self) -> Vec<(String, Vec<i32>)> {
        self.inner.testcase_cancels.borrow().clone()
    }

    pub fn detached_windowed_requests(&self) -> Vec<StartDetachedWindowedEvaluateInput> {
        self.inner.detached_windowed_requests.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::Host;

    fn case(test_case_id: i32) -> StartEvaluateCaseInput {
        StartEvaluateCaseInput {
            problem_id: 1,
            test_case_id,
            solution_source: vec![],
            solution_language: "cpp".to_string(),
            time_limit_ms: 1000,
            memory_limit_kb: 262_144,
            contest_id: None,
            input: Default::default(),
            expected_output: Default::default(),
            is_custom: false,
            target_worker_id: None,
        }
    }

    fn input(ids: impl IntoIterator<Item = i32>) -> StartEvaluateBatchInput {
        StartEvaluateBatchInput {
            problem_type: "icpc".to_string(),
            test_cases: ids.into_iter().map(case).collect(),
        }
    }

    fn batch_test_case_ids(eval: &Eval) -> Vec<Vec<i32>> {
        eval.batch_inputs()
            .into_iter()
            .map(|batch| {
                batch
                    .test_cases
                    .into_iter()
                    .map(|case| case.test_case_id)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn windowed_eval_dispatches_initial_window_and_refills_one_case_per_result() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=10))
            .concurrency(4)
            .start()
            .unwrap();

        assert_eq!(batch_test_case_ids(&host.eval), vec![vec![1, 2, 3, 4]]);

        for completed in 1..=4 {
            host.eval
                .queue_result_for_batch("mock-batch-1", TestCaseVerdict::accepted(completed));
        }
        host.eval
            .queue_result_for_batch("mock-batch-2", TestCaseVerdict::accepted(5));
        host.eval
            .queue_result_for_batch("mock-batch-3", TestCaseVerdict::accepted(6));

        for completed in 1..=6 {
            let result = session.next_result(1234).unwrap().unwrap();
            assert_eq!(result.test_case_id, completed);
        }

        assert_eq!(
            batch_test_case_ids(&host.eval),
            vec![
                vec![1, 2, 3, 4],
                vec![5],
                vec![6],
                vec![7],
                vec![8],
                vec![9],
                vec![10],
            ]
        );
    }

    #[test]
    fn windowed_eval_builder_start_dispatches_initial_window() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=3))
            .concurrency(2)
            .start()
            .unwrap();

        assert_eq!(batch_test_case_ids(&host.eval), vec![vec![1, 2]]);
        host.eval
            .queue_result_for_batch("mock-batch-1", TestCaseVerdict::accepted(1));
        assert_eq!(session.next_result(100).unwrap().unwrap().test_case_id, 1);
    }

    #[test]
    fn windowed_eval_detached_builder_records_typed_request() {
        let host = Host::mock();

        let session_id = host
            .eval
            .windowed(&input(1..=3))
            .concurrency(8)
            .result_timeout_ms(1234)
            .fire_after_judging_for_submission(&OnSubmissionInput {
                submission_id: 42,
                judgement_id: 7,
                fire_after_judging: true,
                user_id: 1,
                problem_id: 2,
                contest_id: None,
                files: Vec::new(),
                language: "cpp".into(),
                time_limit_ms: 1000,
                memory_limit_kb: 262144,
                problem_type: "icpc".into(),
                test_cases: Vec::new(),
                judge_epoch: 9,
                target_worker_id: None,
            })
            .start_detached("on_eval_result", serde_json::json!({ "phase": "icpc" }))
            .unwrap();

        assert_eq!(session_id, "mock-detached-eval-1");
        let requests = host.eval.detached_windowed_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].batch.test_cases.len(), 3);
        assert_eq!(requests[0].concurrency, 8);
        assert_eq!(requests[0].result_timeout_ms, 1234);
        assert_eq!(requests[0].callback_fn, "on_eval_result");
        assert_eq!(requests[0].state["phase"], "icpc");
        assert_eq!(
            requests[0]
                .submission_completion
                .as_ref()
                .map(|completion| completion.submission_id),
            Some(42)
        );
        assert_eq!(
            requests[0]
                .submission_completion
                .as_ref()
                .map(|completion| (completion.judgement_id, completion.judge_epoch)),
            Some((7, 9))
        );
    }

    #[test]
    fn windowed_eval_detached_builder_uses_evaluation_timeout_by_default() {
        let host = Host::mock();

        host.eval
            .windowed(&input(1..=1))
            .start_detached("on_eval_result", serde_json::json!({}))
            .unwrap();

        let requests = host.eval.detached_windowed_requests();
        assert_eq!(
            requests[0].result_timeout_ms,
            default_evaluation_result_timeout_ms(1000)
        );
    }

    #[test]
    fn windowed_eval_reads_ready_later_batch_without_waiting_on_first_batch() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=3))
            .concurrency(2)
            .start()
            .unwrap();

        host.eval
            .queue_result_for_batch("mock-batch-1", TestCaseVerdict::accepted(1));
        assert_eq!(session.next_result(100).unwrap().unwrap().test_case_id, 1);
        assert_eq!(batch_test_case_ids(&host.eval), vec![vec![1, 2], vec![3]]);

        host.eval
            .queue_result_for_batch("mock-batch-2", TestCaseVerdict::accepted(3));
        assert_eq!(session.next_result(100).unwrap().unwrap().test_case_id, 3);
    }

    #[test]
    fn windowed_refill_start_error_does_not_drop_completed_result_or_pending_case() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=3))
            .concurrency(1)
            .start()
            .unwrap();

        host.eval
            .queue_result_for_batch("mock-batch-1", TestCaseVerdict::accepted(1));
        host.eval
            .queue_start_error(SdkError::Other("temporary start failure".into()));

        assert_eq!(session.next_result(100).unwrap().unwrap().test_case_id, 1);
        assert_eq!(batch_test_case_ids(&host.eval), vec![vec![1]]);

        assert!(session.next_result(0).is_err());
        assert!(session.next_result(0).unwrap().is_none());
        assert_eq!(batch_test_case_ids(&host.eval), vec![vec![1], vec![2]]);
    }

    #[test]
    fn windowed_successful_refill_clears_stale_deferred_refill_error() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=4))
            .concurrency(2)
            .start()
            .unwrap();

        host.eval
            .queue_result_for_batch("mock-batch-1", TestCaseVerdict::accepted(1));
        host.eval
            .queue_start_error(SdkError::Other("temporary start failure".into()));
        assert_eq!(session.next_result(100).unwrap().unwrap().test_case_id, 1);

        host.eval
            .queue_result_for_batch("mock-batch-1", TestCaseVerdict::accepted(2));
        assert_eq!(session.next_result(100).unwrap().unwrap().test_case_id, 2);

        assert!(session.next_result(0).unwrap().is_none());
        assert_eq!(
            batch_test_case_ids(&host.eval),
            vec![vec![1, 2], vec![3], vec![4]]
        );
    }

    #[test]
    fn windowed_refill_filter_can_stop_refill_for_terminal_result() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=3))
            .concurrency(1)
            .start()
            .unwrap();

        host.eval
            .queue_result_for_batch("mock-batch-1", TestCaseVerdict::accepted(1));
        assert_eq!(
            session
                .next_result_with_refill_filter(100, |_| false)
                .unwrap()
                .unwrap()
                .test_case_id,
            1
        );

        assert_eq!(batch_test_case_ids(&host.eval), vec![vec![1]]);
    }

    #[test]
    fn window_size_zero_behaves_like_one() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=3))
            .concurrency(0)
            .start()
            .unwrap();

        assert_eq!(batch_test_case_ids(&host.eval), vec![vec![1]]);

        host.eval.queue_result(TestCaseVerdict::accepted(1));
        assert_eq!(session.next_result(100).unwrap().unwrap().test_case_id, 1);
        assert_eq!(batch_test_case_ids(&host.eval), vec![vec![1], vec![2]]);
    }

    #[test]
    fn windowed_cancel_test_cases_groups_active_cancels_drops_pending_and_refills() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=5))
            .concurrency(2)
            .start()
            .unwrap();

        assert_eq!(session.cancel_test_cases(&[2, 4]).unwrap(), 2);

        assert_eq!(
            host.eval.testcase_cancels(),
            vec![("mock-batch-1".to_string(), vec![2])]
        );
        assert_eq!(batch_test_case_ids(&host.eval), vec![vec![1, 2], vec![3]]);
    }

    #[test]
    fn windowed_cancel_all_preserves_active_batch_on_host_error() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=2))
            .concurrency(2)
            .start()
            .unwrap();
        host.eval
            .queue_cancel_error(SdkError::Other("cancel failed".into()));

        assert!(session.cancel_all().is_err());

        host.eval
            .queue_result_for_batch("mock-batch-1", TestCaseVerdict::accepted(1));
        assert_eq!(session.next_result(0).unwrap().unwrap().test_case_id, 1);
    }

    #[test]
    fn windowed_cancel_all_clears_stale_deferred_refill_error() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=3))
            .concurrency(2)
            .start()
            .unwrap();

        host.eval
            .queue_result_for_batch("mock-batch-1", TestCaseVerdict::accepted(1));
        host.eval
            .queue_start_error(SdkError::Other("temporary start failure".into()));
        assert_eq!(session.next_result(100).unwrap().unwrap().test_case_id, 1);

        session.cancel_all().unwrap();

        assert!(session.next_result(0).unwrap().is_none());
    }

    #[test]
    fn windowed_cancel_test_cases_preserves_pending_and_failed_active_on_host_error() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=4))
            .concurrency(2)
            .start()
            .unwrap();
        host.eval
            .queue_testcase_cancel_result(Err(SdkError::Other("cancel failed".into())));

        assert!(session.cancel_test_cases(&[2, 3]).is_err());

        host.eval
            .queue_result_for_batch("mock-batch-1", TestCaseVerdict::accepted(2));
        assert_eq!(session.next_result(0).unwrap().unwrap().test_case_id, 2);
        assert_eq!(batch_test_case_ids(&host.eval), vec![vec![1, 2], vec![3]]);
    }

    #[test]
    fn windowed_cancel_test_cases_preserves_active_ids_on_count_mismatch() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=2))
            .concurrency(2)
            .start()
            .unwrap();
        host.eval.queue_testcase_cancel_result(Ok(1));

        assert!(session.cancel_test_cases(&[1, 2]).is_err());

        host.eval
            .queue_result_for_batch("mock-batch-1", TestCaseVerdict::accepted(1));
        assert_eq!(session.next_result(0).unwrap().unwrap().test_case_id, 1);
    }

    #[test]
    fn windowed_cancel_test_cases_cancels_multi_active_batches() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=4))
            .concurrency(2)
            .start()
            .unwrap();

        host.eval.queue_result(TestCaseVerdict::accepted(1));
        assert_eq!(session.next_result(100).unwrap().unwrap().test_case_id, 1);
        host.eval.queue_result(TestCaseVerdict::accepted(2));
        assert_eq!(session.next_result(100).unwrap().unwrap().test_case_id, 2);

        assert_eq!(session.cancel_test_cases(&[3, 4]).unwrap(), 2);
        assert_eq!(
            host.eval.testcase_cancels(),
            vec![
                ("mock-batch-2".to_string(), vec![3]),
                ("mock-batch-3".to_string(), vec![4]),
            ]
        );
        assert!(session.next_result(0).unwrap().is_none());
    }

    #[test]
    fn windowed_cancel_test_cases_keeps_state_after_cleanup_error() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=2))
            .concurrency(2)
            .start()
            .unwrap();
        host.eval
            .queue_cancel_error(SdkError::Other("batch cleanup failed".into()));

        assert_eq!(session.cancel_test_cases(&[1, 2]).unwrap(), 2);
        assert!(session.next_result(0).unwrap().is_none());
    }

    #[test]
    fn windowed_cancel_test_cases_defers_refill_error_after_success() {
        let host = Host::mock();
        let mut session = host
            .eval
            .windowed(&input(1..=2))
            .concurrency(1)
            .start()
            .unwrap();
        host.eval
            .queue_start_error(SdkError::Other("temporary start failure".into()));

        assert_eq!(session.cancel_test_cases(&[1]).unwrap(), 1);
        assert!(session.next_result(0).is_err());
    }

    #[test]
    fn eval_mock_global_results_still_work_after_batch_results_are_drained() {
        let host = Host::mock();
        host.eval
            .queue_result_for_batch("mock-batch-1", TestCaseVerdict::accepted(1));
        assert_eq!(
            host.eval
                .next_result("mock-batch-1", 0)
                .unwrap()
                .unwrap()
                .test_case_id,
            1
        );

        host.eval.queue_result(TestCaseVerdict::accepted(2));
        assert_eq!(
            host.eval
                .next_result("mock-batch-2", 0)
                .unwrap()
                .unwrap()
                .test_case_id,
            2
        );
    }
}
