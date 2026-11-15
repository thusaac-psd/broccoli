use crate::error::SdkError;
use crate::types::{OperationResult, OperationTask};
use std::collections::VecDeque;
use std::time::Instant;

pub struct Operations {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) inner: OperationsMock,
}

pub struct WindowedOperationBuilder<'a> {
    operations: &'a Operations,
    tasks: &'a [OperationTask],
    concurrency: usize,
}

impl Operations {
    pub fn windowed<'a>(&'a self, tasks: &'a [OperationTask]) -> WindowedOperationBuilder<'a> {
        WindowedOperationBuilder {
            operations: self,
            tasks,
            concurrency: 1,
        }
    }
}

impl<'a> WindowedOperationBuilder<'a> {
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    pub fn collect(self, timeout_ms: u64) -> Result<Vec<OperationResult>, SdkError> {
        if self.tasks.is_empty() {
            return Ok(Vec::new());
        }

        let started = Instant::now();
        let mut pending = self
            .tasks
            .iter()
            .cloned()
            .enumerate()
            .collect::<VecDeque<_>>();
        let mut active = VecDeque::<(usize, String)>::new();
        let mut results = vec![None; self.tasks.len()];

        if let Err(err) = self.refill_active(&mut pending, &mut active) {
            let _ = self.cancel_active(active.iter().map(|(_, batch_id)| batch_id.as_str()));
            return Err(err);
        }

        while let Some((index, batch_id)) = active.pop_front() {
            let remaining_ms = remaining_timeout_ms(started, timeout_ms).ok_or_else(|| {
                let _ = self.cancel_active(active.iter().map(|(_, batch_id)| batch_id.as_str()));
                SdkError::HostCall(format!(
                    "Operation window timed out waiting for {} result(s)",
                    self.tasks.len()
                ))
            })?;

            match self.operations.next_result(&batch_id, remaining_ms) {
                Ok(result) => {
                    results[index] = Some(result);
                    if let Err(err) = self.refill_active(&mut pending, &mut active) {
                        let _ = self
                            .cancel_active(active.iter().map(|(_, batch_id)| batch_id.as_str()));
                        return Err(err);
                    }
                }
                Err(err) => {
                    let _ = self.operations.cancel_batch(&batch_id);
                    let _ =
                        self.cancel_active(active.iter().map(|(_, batch_id)| batch_id.as_str()));
                    return Err(err);
                }
            }
        }

        Ok(results
            .into_iter()
            .map(|result| result.expect("all operation results were filled"))
            .collect())
    }

    fn refill_active(
        &self,
        pending: &mut VecDeque<(usize, OperationTask)>,
        active: &mut VecDeque<(usize, String)>,
    ) -> Result<(), SdkError> {
        while active.len() < self.concurrency {
            let Some((index, task)) = pending.pop_front() else {
                break;
            };
            let batch_id = self.operations.start_batch(std::slice::from_ref(&task))?;
            active.push_back((index, batch_id));
        }
        Ok(())
    }

    fn cancel_active<'b>(
        &self,
        batch_ids: impl IntoIterator<Item = &'b str>,
    ) -> Result<(), SdkError> {
        let mut first_error = None;
        for batch_id in batch_ids {
            if let Err(err) = self.operations.cancel_batch(batch_id)
                && first_error.is_none()
            {
                first_error = Some(err);
            }
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

fn remaining_timeout_ms(started: Instant, timeout_ms: u64) -> Option<u64> {
    if timeout_ms == 0 {
        return Some(0);
    }
    let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let remaining_ms = timeout_ms.saturating_sub(elapsed_ms);
    (remaining_ms > 0).then_some(remaining_ms)
}

#[cfg(target_arch = "wasm32")]
impl Operations {
    fn start_batch(&self, tasks: &[OperationTask]) -> Result<String, SdkError> {
        let ops_json = serde_json::to_string(tasks)?;
        let response_json = unsafe { crate::host::raw::start_operation_batch(ops_json)? };
        let response: serde_json::Value = serde_json::from_str(&response_json)?;
        response["batch_id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| {
                SdkError::HostCall("Missing batch_id in start_operation_batch response".into())
            })
    }

    fn next_result(&self, batch_id: &str, timeout_ms: u64) -> Result<OperationResult, SdkError> {
        let input = serde_json::json!({
            "batch_id": batch_id,
            "timeout_ms": timeout_ms,
        });
        let result_json =
            unsafe { crate::host::raw::get_next_operation_result(serde_json::to_string(&input)?)? };
        let envelope: serde_json::Value = serde_json::from_str(&result_json)?;

        let task_result = envelope
            .get("result")
            .filter(|v| !v.is_null())
            .ok_or_else(|| SdkError::HostCall("Operation timed out — no result received".into()))?;

        let output = task_result
            .get("output")
            .ok_or_else(|| SdkError::HostCall("Missing 'output' in TaskResult".into()))?;

        serde_json::from_value::<OperationResult>(output.clone()).map_err(|_| {
            let error_msg = output
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("Unknown operation error");
            SdkError::HostCall(format!("Operation failed at worker: {error_msg}"))
        })
    }

    fn cancel_batch(&self, batch_id: &str) -> Result<(), SdkError> {
        let input = serde_json::json!({ "batch_id": batch_id });
        unsafe { crate::host::raw::cancel_operation_batch(serde_json::to_string(&input)?)? };
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
use std::cell::RefCell;

#[cfg(not(target_arch = "wasm32"))]
pub(super) struct OperationsMock {
    batch_inputs: RefCell<Vec<Vec<OperationTask>>>,
    results: RefCell<Vec<OperationResult>>,
    result_timeouts: RefCell<Vec<u64>>,
    cancels: RefCell<Vec<String>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl OperationsMock {
    pub fn new() -> Self {
        Self {
            batch_inputs: RefCell::new(Vec::new()),
            results: RefCell::new(Vec::new()),
            result_timeouts: RefCell::new(Vec::new()),
            cancels: RefCell::new(Vec::new()),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Operations {
    fn start_batch(&self, tasks: &[OperationTask]) -> Result<String, SdkError> {
        self.inner.batch_inputs.borrow_mut().push(tasks.to_vec());
        Ok(format!(
            "mock-op-batch-{}",
            self.inner.batch_inputs.borrow().len()
        ))
    }

    fn next_result(&self, batch_id: &str, timeout_ms: u64) -> Result<OperationResult, SdkError> {
        self.inner.result_timeouts.borrow_mut().push(timeout_ms);
        if self.inner.results.borrow().is_empty() {
            return Err(SdkError::HostCall(format!(
                "Operation batch {batch_id} timed out — no result received"
            )));
        }
        Ok(self.inner.results.borrow_mut().remove(0))
    }

    fn cancel_batch(&self, batch_id: &str) -> Result<(), SdkError> {
        self.inner.cancels.borrow_mut().push(batch_id.to_string());
        Ok(())
    }

    pub fn queue_result(&self, result: OperationResult) {
        self.inner.results.borrow_mut().push(result);
    }

    pub fn batch_inputs(&self) -> Vec<Vec<OperationTask>> {
        self.inner.batch_inputs.borrow().clone()
    }

    pub fn result_timeouts(&self) -> Vec<u64> {
        self.inner.result_timeouts.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use crate::Host;
    use crate::types::{OperationResult, OperationTask, Step};

    fn operation() -> OperationTask {
        OperationTask {
            environments: vec![],
            tasks: vec![Step {
                id: "run".to_string(),
                kind: Default::default(),
                env_ref: "env".to_string(),
                argv: vec!["true".to_string()],
                conf: Default::default(),
                io: Default::default(),
                collect: vec![],
                depends_on: vec![],
                cache: None,
                mounts: vec![],
            }],
            channels: vec![],
            target_worker_id: None,
            evaluate_batch_id: None,
            test_case_id: None,
        }
    }

    #[test]
    fn windowed_operation_collect_dispatches_and_drains_batch() {
        let host = Host::mock();
        let ops = vec![operation(), operation(), operation()];
        host.operations.queue_result(OperationResult {
            success: true,
            task_results: Default::default(),
            error: Some("first".to_string()),
        });
        host.operations.queue_result(OperationResult {
            success: true,
            task_results: Default::default(),
            error: Some("second".to_string()),
        });
        host.operations.queue_result(OperationResult {
            success: true,
            task_results: Default::default(),
            error: Some("third".to_string()),
        });

        let results = host
            .operations
            .windowed(&ops)
            .concurrency(2)
            .collect(1234)
            .unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(
            results
                .iter()
                .map(|result| result.error.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("first"), Some("second"), Some("third")]
        );
        assert_eq!(host.operations.batch_inputs().len(), 3);
        assert_eq!(host.operations.batch_inputs()[0].len(), 1);
        assert_eq!(host.operations.result_timeouts().len(), 3);
    }
}
