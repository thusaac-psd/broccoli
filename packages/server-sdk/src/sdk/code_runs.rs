#[cfg(not(target_arch = "wasm32"))]
use std::cell::RefCell;
#[cfg(not(target_arch = "wasm32"))]
use std::collections::VecDeque;

use crate::error::SdkError;
use crate::types::{CodeRunResultRow, CodeRunUpdate, SubmissionStatus};

pub struct CodeRuns {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) inner: CodeRunsMock,
}

impl CodeRuns {
    pub fn set_compiling(&self, code_run_id: i32, judge_epoch: i32) -> Result<(), SdkError> {
        self.update(&CodeRunUpdate {
            code_run_id,
            judge_epoch,
            status: Some(SubmissionStatus::Compiling),
            ..Default::default()
        })
    }

    pub fn set_running(&self, code_run_id: i32, judge_epoch: i32) -> Result<(), SdkError> {
        self.update(&CodeRunUpdate {
            code_run_id,
            judge_epoch,
            status: Some(SubmissionStatus::Running),
            ..Default::default()
        })
    }
}

#[cfg(target_arch = "wasm32")]
impl CodeRuns {
    /// Writes `update` onto the code run. The host owns the statement and
    /// fences it on the run's epoch while it is in flight, so a write from a
    /// dispatch the host has since requeued changes nothing.
    pub fn update(&self, update: &CodeRunUpdate) -> Result<(), SdkError> {
        let payload = serde_json::to_string(update)?;
        let result_json = unsafe { crate::host::raw::code_run_update(payload)? };
        let resp: super::shared::HostDbResponse = serde_json::from_str(&result_json)?;
        resp.into_result()?;
        Ok(())
    }

    /// Inserts per-run results, behind the same fence as [`Self::update`].
    pub fn insert_results(&self, results: &[CodeRunResultRow]) -> Result<(), SdkError> {
        if results.is_empty() {
            return Ok(());
        }
        let payload = serde_json::to_string(results)?;
        let result_json = unsafe { crate::host::raw::code_run_insert_results(payload)? };
        let resp: super::shared::HostDbResponse = serde_json::from_str(&result_json)?;
        resp.into_result()?;
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) struct CodeRunsMock {
    update_errors: RefCell<VecDeque<SdkError>>,
    insert_errors: RefCell<VecDeque<SdkError>>,
    updates: RefCell<Vec<CodeRunUpdate>>,
    results: RefCell<Vec<CodeRunResultRow>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl CodeRunsMock {
    pub fn new() -> Self {
        Self {
            update_errors: RefCell::new(VecDeque::new()),
            insert_errors: RefCell::new(VecDeque::new()),
            updates: RefCell::new(Vec::new()),
            results: RefCell::new(Vec::new()),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl CodeRuns {
    pub fn update(&self, update: &CodeRunUpdate) -> Result<(), SdkError> {
        if let Some(err) = self.inner.update_errors.borrow_mut().pop_front() {
            return Err(err);
        }
        self.inner.updates.borrow_mut().push(update.clone());
        Ok(())
    }

    pub fn insert_results(&self, results: &[CodeRunResultRow]) -> Result<(), SdkError> {
        if let Some(err) = self.inner.insert_errors.borrow_mut().pop_front() {
            return Err(err);
        }
        self.inner.results.borrow_mut().extend_from_slice(results);
        Ok(())
    }

    pub fn queue_update_error(&self, err: SdkError) {
        self.inner.update_errors.borrow_mut().push_back(err);
    }

    pub fn queue_insert_error(&self, err: SdkError) {
        self.inner.insert_errors.borrow_mut().push_back(err);
    }

    pub fn updates(&self) -> Vec<CodeRunUpdate> {
        self.inner.updates.borrow().clone()
    }

    pub fn last_update(&self) -> CodeRunUpdate {
        let updates = self.inner.updates.borrow();
        assert!(!updates.is_empty(), "Expected at least 1 code_run update");
        updates.last().unwrap().clone()
    }

    pub fn results(&self) -> Vec<CodeRunResultRow> {
        self.inner.results.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use crate::sdk::Host;
    use crate::types::SubmissionStatus;

    #[test]
    fn status_helpers_write_compiling_and_running_updates() {
        let host = Host::mock();

        host.code_run.set_compiling(10, 30).unwrap();
        host.code_run.set_running(10, 30).unwrap();

        let updates = host.code_run.updates();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].code_run_id, 10);
        assert_eq!(updates[0].judge_epoch, 30);
        assert_eq!(updates[0].status, Some(SubmissionStatus::Compiling));
        assert_eq!(updates[1].status, Some(SubmissionStatus::Running));
    }
}
