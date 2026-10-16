#[cfg(not(target_arch = "wasm32"))]
use crate::types::SubmissionSlotToken;
use crate::types::{
    FeedbackUpdateOutcome, ReleaseResult, SubmissionFeedbackAction, SubmissionFeedbackQuery,
    SubmissionFeedbackRecord, SubmissionSlotObservation,
};
#[cfg(not(target_arch = "wasm32"))]
use std::cell::RefCell;
#[cfg(not(target_arch = "wasm32"))]
use std::collections::VecDeque;

use crate::error::SdkError;
#[cfg(not(target_arch = "wasm32"))]
use crate::types::TestCaseBodyRef;
use crate::types::{SubmissionStatus, SubmissionUpdate, TestCaseResultRow, TestCaseRow};

pub struct Submissions {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) inner: SubmissionsMock,
}

impl Submissions {
    pub fn set_compiling(
        &self,
        submission_id: i32,
        judgement_id: i32,
        judge_epoch: i32,
    ) -> Result<u64, SdkError> {
        self.update(&SubmissionUpdate {
            submission_id,
            judgement_id,
            judge_epoch,
            status: Some(SubmissionStatus::Compiling),
            ..Default::default()
        })
    }

    pub fn set_running(
        &self,
        submission_id: i32,
        judgement_id: i32,
        judge_epoch: i32,
    ) -> Result<u64, SdkError> {
        self.update(&SubmissionUpdate {
            submission_id,
            judgement_id,
            judge_epoch,
            status: Some(SubmissionStatus::Running),
            ..Default::default()
        })
    }
}

#[cfg(target_arch = "wasm32")]
impl Submissions {
    pub fn query_test_cases(&self, problem_id: i32) -> Result<Vec<TestCaseRow>, SdkError> {
        // The server owns the SQL now: serialize the structured input and call
        // the capability-gated `submission_query_test_cases` host fn, which
        // returns the same `HostDbResponse` envelope `db_query` did.
        let payload = serde_json::to_string(&problem_id)?;
        let result_json = unsafe { crate::host::raw::submission_query_test_cases(payload)? };
        let resp: super::shared::HostDbResponse = serde_json::from_str(&result_json)?;
        super::shared::parse_rows(resp.into_result()?)
    }

    pub fn update(&self, update: &SubmissionUpdate) -> Result<u64, SdkError> {
        // Pre-DB guard preserved from the old SQL-building path: a non-positive
        // judgement id is a stale epoch and must not reach the server.
        if update.judgement_id <= 0 {
            return Err(SdkError::StaleEpoch);
        }

        // The server reproduces the exact `submission_judgement` + `submission`
        // statements (including the empty-sets and 0-rows short-circuits) from
        // this structured input and returns rows-affected in the standard
        // envelope.
        let payload = serde_json::to_string(update)?;
        let result_json = unsafe { crate::host::raw::submission_update(payload)? };
        let resp: super::shared::HostDbResponse = serde_json::from_str(&result_json)?;
        Ok(super::shared::parse_affected(resp.into_result()?))
    }

    pub fn insert_results(&self, results: &[TestCaseResultRow]) -> Result<(), SdkError> {
        // Match the old early-return and per-row stale-epoch guards before the
        // host call; the server owns the INSERT + result-text sanitization.
        if results.is_empty() {
            return Ok(());
        }
        for r in results {
            if r.judgement_id <= 0 {
                return Err(SdkError::StaleEpoch);
            }
        }

        let payload = serde_json::to_string(results)?;
        let result_json = unsafe { crate::host::raw::submission_insert_results(payload)? };
        let resp: super::shared::HostDbResponse = serde_json::from_str(&result_json)?;
        // Propagate any DB error; the affected-row count is not used here.
        resp.into_result()?;
        Ok(())
    }

    pub fn delete_results(
        &self,
        submission_id: i32,
        judgement_id: i32,
        judge_epoch: i32,
    ) -> Result<(), SdkError> {
        if judgement_id <= 0 {
            return Err(SdkError::StaleEpoch);
        }

        let payload = serde_json::to_string(&(submission_id, judgement_id, judge_epoch))?;
        let result_json = unsafe { crate::host::raw::submission_delete_results(payload)? };
        let resp: super::shared::HostDbResponse = serde_json::from_str(&result_json)?;
        resp.into_result()?;
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) struct SubmissionsMock {
    feedback: RefCell<Vec<SubmissionFeedbackRecord>>,
    feedback_actions: RefCell<Vec<(i32, SubmissionFeedbackAction)>>,
    feedback_errors: RefCell<VecDeque<SdkError>>,
    feedback_query_errors: RefCell<VecDeque<Option<SdkError>>>,
    test_cases: RefCell<Vec<TestCaseRow>>,
    update_results: RefCell<VecDeque<Result<u64, SdkError>>>,
    query_errors: RefCell<VecDeque<SdkError>>,
    insert_errors: RefCell<VecDeque<SdkError>>,
    updates: RefCell<Vec<SubmissionUpdate>>,
    tc_results: RefCell<Vec<TestCaseResultRow>>,
    /// Number of `insert_results` calls that actually issued an INSERT
    /// (empty-slice calls early-return and are not counted). Used by the
    /// UP#34 bulk-insert-results regression tests to assert that plugins
    /// batch their per-testcase rows rather than calling once per row.
    insert_call_count: RefCell<usize>,
}

#[cfg(not(target_arch = "wasm32"))]
impl SubmissionsMock {
    pub fn new() -> Self {
        Self {
            test_cases: RefCell::new(Vec::new()),
            feedback: RefCell::new(Vec::new()),
            feedback_actions: RefCell::new(Vec::new()),
            feedback_errors: RefCell::new(VecDeque::new()),
            feedback_query_errors: RefCell::new(VecDeque::new()),
            update_results: RefCell::new(VecDeque::new()),
            query_errors: RefCell::new(VecDeque::new()),
            insert_errors: RefCell::new(VecDeque::new()),
            updates: RefCell::new(Vec::new()),
            tc_results: RefCell::new(Vec::new()),
            insert_call_count: RefCell::new(0),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Submissions {
    pub fn query_test_cases(&self, _problem_id: i32) -> Result<Vec<TestCaseRow>, SdkError> {
        if let Some(err) = self.inner.query_errors.borrow_mut().pop_front() {
            return Err(err);
        }
        Ok(self.inner.test_cases.borrow().clone())
    }

    pub fn update(&self, update: &SubmissionUpdate) -> Result<u64, SdkError> {
        if update.judgement_id <= 0 {
            return Err(SdkError::StaleEpoch);
        }
        self.inner.updates.borrow_mut().push(update.clone());
        if let Some(result) = self.inner.update_results.borrow_mut().pop_front() {
            return result;
        }
        Ok(1)
    }

    pub fn insert_results(&self, results: &[TestCaseResultRow]) -> Result<(), SdkError> {
        if results.is_empty() {
            return Ok(());
        }
        if results.iter().any(|r| r.judgement_id <= 0) {
            return Err(SdkError::StaleEpoch);
        }
        if let Some(err) = self.inner.insert_errors.borrow_mut().pop_front() {
            return Err(err);
        }
        *self.inner.insert_call_count.borrow_mut() += 1;
        self.inner
            .tc_results
            .borrow_mut()
            .extend_from_slice(results);
        Ok(())
    }

    pub fn delete_results(
        &self,
        submission_id: i32,
        judgement_id: i32,
        judge_epoch: i32,
    ) -> Result<(), SdkError> {
        if judgement_id <= 0 {
            return Err(SdkError::StaleEpoch);
        }
        self.inner.tc_results.borrow_mut().retain(|r| {
            r.submission_id != submission_id
                || r.judgement_id != judgement_id
                || r.judge_epoch != judge_epoch
        });
        Ok(())
    }

    pub fn set_test_cases(&self, tcs: Vec<TestCaseRow>) {
        *self.inner.test_cases.borrow_mut() = tcs;
    }

    pub fn add_test_case(&self, id: i32, score: f64) {
        let pos = self.inner.test_cases.borrow().len() as i32;
        self.inner.test_cases.borrow_mut().push(TestCaseRow {
            id,
            score,
            is_sample: false,
            position: pos,
            description: None,
            label: Some(id.to_string()),
            input: TestCaseBodyRef::Missing,
            expected_output: TestCaseBodyRef::Missing,
            is_custom: false,
        });
    }

    pub fn queue_update_result(&self, result: Result<u64, SdkError>) {
        self.inner.update_results.borrow_mut().push_back(result);
    }

    pub fn queue_query_error(&self, err: SdkError) {
        self.inner.query_errors.borrow_mut().push_back(err);
    }

    pub fn queue_insert_error(&self, err: SdkError) {
        self.inner.insert_errors.borrow_mut().push_back(err);
    }

    pub fn updates(&self) -> Vec<SubmissionUpdate> {
        self.inner.updates.borrow().clone()
    }

    pub fn last_update(&self) -> SubmissionUpdate {
        let updates = self.inner.updates.borrow();
        assert!(!updates.is_empty(), "Expected at least 1 submission update");
        updates.last().unwrap().clone()
    }

    pub fn results(&self) -> Vec<TestCaseResultRow> {
        self.inner.tc_results.borrow().clone()
    }

    /// Number of `insert_results` calls that actually issued an INSERT.
    /// Empty-slice calls early-return and are not counted. Plugins are
    /// expected to batch per-testcase rows (UP#34) - assert
    /// `insert_call_count() <= ceil(rows / threshold) + small_constant` in
    /// regression tests.
    pub fn insert_call_count(&self) -> usize {
        *self.inner.insert_call_count.borrow()
    }
}

impl Submissions {
    /// Explicitly read every page matching a query. `feedback` reads one bounded
    /// page; its limit is the page size here, not a total result limit.
    /// Pages are read independently; concurrent changes are not a snapshot.
    pub fn feedback_all(
        &self,
        query: &SubmissionFeedbackQuery,
    ) -> Result<Vec<SubmissionFeedbackRecord>, SdkError> {
        let mut query = query.clone();
        let mut records = Vec::new();
        loop {
            let page = self.feedback(&query)?;
            let full = page.len() == query.page_size() as usize;
            if let Some(last) = page.last() {
                query.after_id = last.submission_id;
            }
            records.extend(page);
            if !full {
                return Ok(records);
            }
        }
    }

    /// Reveal the raw judgement or a plugin-selected public verdict and score.
    /// This also releases the slot. Identical retries succeed; conflicts error.
    /// Original follows the raw judgement, including later rejudges. An override
    /// fixes verdict and score when results are present; lifecycle and testcase
    /// data still follow raw rejudges. Rejudging does not reacquire the slot or
    /// withhold released feedback again.
    pub fn release_feedback(
        &self,
        submission_id: i32,
        result: ReleaseResult,
    ) -> Result<FeedbackUpdateOutcome, SdkError> {
        // JSON encodes non-finite floats as null. Reject them before crossing
        // WASM so the host cannot trap while decoding the typed score.
        result
            .validate()
            .map_err(|e| SdkError::Database(e.into()))?;
        self.update_feedback(submission_id, &SubmissionFeedbackAction::Release(result))
    }

    /// Unblock further submissions without revealing the withheld result.
    pub fn release_slot(&self, submission_id: i32) -> Result<FeedbackUpdateOutcome, SdkError> {
        self.update_feedback(submission_id, &SubmissionFeedbackAction::ReleaseSlot)
    }
}

#[cfg(target_arch = "wasm32")]
impl Submissions {
    /// Read one bounded page of this plugin's feedback records, ordered by ID.
    pub fn feedback(
        &self,
        query: &SubmissionFeedbackQuery,
    ) -> Result<Vec<SubmissionFeedbackRecord>, SdkError> {
        let json =
            unsafe { crate::host::raw::submission_feedback_query(serde_json::to_string(query)?)? };
        let response: super::shared::HostDbResponse = serde_json::from_str(&json)?;
        super::shared::parse_rows(response.into_result()?)
    }

    /// Observe before reading policy inputs; admission checks the token again.
    pub fn observe_slot(&self, key: &str) -> Result<SubmissionSlotObservation, SdkError> {
        let json =
            unsafe { crate::host::raw::submission_slot_observe(serde_json::to_string(key)?)? };
        let response: super::shared::HostDbResponse = serde_json::from_str(&json)?;
        Ok(serde_json::from_value(
            response
                .into_result()?
                .ok_or_else(|| SdkError::Other("Missing slot observation".into()))?,
        )?)
    }

    fn update_feedback(
        &self,
        submission_id: i32,
        action: &SubmissionFeedbackAction,
    ) -> Result<FeedbackUpdateOutcome, SdkError> {
        let input = serde_json::json!({"submission_id":submission_id,"action":action});
        let json = unsafe { crate::host::raw::submission_feedback_update(input.to_string())? };
        let response: super::shared::HostDbResponse = serde_json::from_str(&json)?;
        Ok(serde_json::from_value(
            response
                .into_result()?
                .ok_or_else(|| SdkError::Other("Missing feedback update outcome".into()))?,
        )?)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Submissions {
    pub fn feedback(
        &self,
        query: &SubmissionFeedbackQuery,
    ) -> Result<Vec<SubmissionFeedbackRecord>, SdkError> {
        if let Some(error) = self
            .inner
            .feedback_query_errors
            .borrow_mut()
            .pop_front()
            .flatten()
        {
            return Err(error);
        }
        Ok(self
            .inner
            .feedback
            .borrow()
            .iter()
            .filter(|r| {
                r.submission_id > query.after_id
                    && query.key.as_ref().is_none_or(|key| key == &r.key)
                    && query
                        .slot_key
                        .as_ref()
                        .is_none_or(|key| Some(key) == r.slot_key.as_ref())
                    && query
                        .submission_ids
                        .as_ref()
                        .is_none_or(|ids| ids.contains(&r.submission_id))
                    && (!query.unpublished_only || r.withheld)
            })
            .take(query.page_size() as usize)
            .cloned()
            .collect())
    }

    pub fn observe_slot(&self, key: &str) -> Result<SubmissionSlotObservation, SdkError> {
        let records = self.inner.feedback.borrow();
        let matching = || {
            records
                .iter()
                .filter(|r| r.slot_key.as_deref() == Some(key))
        };
        Ok(SubmissionSlotObservation {
            token: SubmissionSlotToken::new(
                key.to_owned(),
                matching().map(|r| r.submission_id).max(),
            ),
            pending: matching().find(|r| r.slot_held).cloned(),
        })
    }

    fn update_feedback(
        &self,
        submission_id: i32,
        action: &SubmissionFeedbackAction,
    ) -> Result<FeedbackUpdateOutcome, SdkError> {
        if let Some(error) = self.inner.feedback_errors.borrow_mut().pop_front() {
            return Err(error);
        }
        let mut records = self.inner.feedback.borrow_mut();
        let record = records
            .iter_mut()
            .find(|r| r.submission_id == submission_id)
            .ok_or_else(|| {
                SdkError::Database("Feedback record not found for this plugin".into())
            })?;
        let outcome = record
            .apply_action(action)
            .map_err(|e| SdkError::Database(e.into()))?;
        self.inner
            .feedback_actions
            .borrow_mut()
            .push((submission_id, action.clone()));
        Ok(outcome)
    }

    pub fn set_feedback(&self, mut records: Vec<SubmissionFeedbackRecord>) {
        records.sort_by_key(|r| r.submission_id);
        *self.inner.feedback.borrow_mut() = records;
    }
    pub fn queue_feedback_query_success(&self) {
        self.inner
            .feedback_query_errors
            .borrow_mut()
            .push_back(None);
    }
    pub fn queue_feedback_query_error(&self, error: SdkError) {
        self.inner
            .feedback_query_errors
            .borrow_mut()
            .push_back(Some(error));
    }
    pub fn queue_feedback_error(&self, error: SdkError) {
        self.inner.feedback_errors.borrow_mut().push_back(error);
    }
    pub fn feedback_actions(&self) -> Vec<(i32, SubmissionFeedbackAction)> {
        self.inner.feedback_actions.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use crate::sdk::Host;
    use crate::types::SubmissionStatus;

    #[test]
    fn feedback_releases_are_idempotent_final_and_release_slots_independently() {
        use crate::types::*;
        let host = Host::mock();
        host.submission.set_feedback(vec![SubmissionFeedbackRecord {
            submission_id: 7,
            withheld: true,
            slot_held: true,
            slot_key: Some("player".into()),
            ..Default::default()
        }]);
        assert!(
            host.submission
                .observe_slot("player")
                .unwrap()
                .pending
                .is_some()
        );
        host.submission
            .queue_feedback_error(crate::error::SdkError::Database(
                "transport unavailable".into(),
            ));
        for score in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let error = host
                .submission
                .release_feedback(
                    7,
                    ReleaseResult::Override {
                        verdict: Verdict::Accepted,
                        score,
                    },
                )
                .unwrap_err();
            assert!(error.to_string().contains("Invalid feedback score"));
        }
        // Invalid requests must fail before the transport on both targets.
        assert!(
            host.submission
                .release_slot(7)
                .unwrap_err()
                .to_string()
                .contains("transport unavailable")
        );
        assert!(
            host.submission
                .observe_slot("player")
                .unwrap()
                .pending
                .is_some()
        );
        assert_eq!(
            host.submission.release_slot(7).unwrap(),
            FeedbackUpdateOutcome::Applied
        );
        assert_eq!(
            host.submission.release_slot(7).unwrap(),
            FeedbackUpdateOutcome::AlreadyApplied
        );
        let slot = host.submission.observe_slot("player").unwrap();
        assert!(slot.pending.is_none());
        assert_eq!(slot.token.generation(), Some(7));
        assert!(
            host.submission
                .feedback(&SubmissionFeedbackQuery::default())
                .unwrap()[0]
                .withheld
        );
        let result = ReleaseResult::Override {
            verdict: Verdict::Other("Custom".into()),
            score: 1.0,
        };
        assert_eq!(
            host.submission.release_feedback(7, result.clone()).unwrap(),
            FeedbackUpdateOutcome::Applied
        );
        assert_eq!(
            host.submission.release_feedback(7, result).unwrap(),
            FeedbackUpdateOutcome::AlreadyApplied
        );
        assert!(
            host.submission
                .release_feedback(7, ReleaseResult::Original)
                .is_err()
        );
        assert!(host.submission.release_slot(99).is_err());
        let record = host
            .submission
            .feedback(&SubmissionFeedbackQuery::default())
            .unwrap()
            .remove(0);
        assert!(!record.withheld && !record.slot_held);
        assert_eq!(
            record.result.unwrap().verdict,
            Verdict::Other("Custom".into())
        );
    }

    #[test]
    fn feedback_queries_are_bounded_and_all_pages_are_explicit() {
        use crate::types::*;
        let host = Host::mock();
        host.submission.set_feedback(
            (1..=205)
                .rev()
                .map(|id| SubmissionFeedbackRecord {
                    submission_id: id,
                    key: "match".into(),
                    withheld: true,
                    ..Default::default()
                })
                .collect(),
        );
        let mut query = SubmissionFeedbackQuery::default();
        let page = host.submission.feedback(&query).unwrap();
        assert_eq!(page.len(), 100);
        assert_eq!(page[0].submission_id, 1);
        let all = host.submission.feedback_all(&query).unwrap();
        assert_eq!(all.len(), 205);
        assert_eq!(all.last().unwrap().submission_id, 205);
        query.after_id = 100;
        query.limit = 0;
        assert_eq!(host.submission.feedback(&query).unwrap().len(), 1);
        query.limit = u32::MAX;
        assert_eq!(query.page_size(), 1000);
        query.key = Some("another-match".into());
        assert!(host.submission.feedback_all(&query).unwrap().is_empty());
    }

    #[test]
    fn status_helpers_write_compiling_and_running_updates() {
        let host = Host::mock();

        assert_eq!(host.submission.set_compiling(10, 20, 30).unwrap(), 1);
        assert_eq!(host.submission.set_running(10, 20, 30).unwrap(), 1);

        let updates = host.submission.updates();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].submission_id, 10);
        assert_eq!(updates[0].judgement_id, 20);
        assert_eq!(updates[0].judge_epoch, 30);
        assert_eq!(updates[0].status, Some(SubmissionStatus::Compiling));
        assert_eq!(updates[1].status, Some(SubmissionStatus::Running));
    }
}
