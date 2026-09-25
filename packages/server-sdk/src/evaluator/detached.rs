//! Shared driver for detached windowed evaluation.
//!
//! A contest-type plugin (ICPC, IOI, ...) judges a submission by streaming its
//! test cases through the host's windowed evaluator and reacting to each result
//! as it lands. The *mechanics* of that loop are identical across contest types
//! and safety-critical to get right: the epoch-guarded status transitions, the
//! per-case persistence, the compile-error short-circuit, the re-delivered-case
//! idempotency, the timeout fill, and the exact host-protocol reply shape
//! (`refill` / `cancel_test_case_ids` / action). Only two things actually differ
//! between contest types: how a single case is *scored*, and when the batch
//! *stops early*.
//!
//! This module owns every mechanic and asks the contest type for only the two
//! policy decisions, via [`ContestJudge`]. The design goal is that a plugin
//! author never has to reason about the host callback protocol - in particular
//! never touches the `refill` hint, whose wrong value silently strands pending
//! cases as `SystemError`. The driver computes it.
//!
//! ## What the driver guarantees (so the policy needn't)
//!
//! - **Epoch safety.** `set_compiling` / `set_running` are gated on the judge
//!   epoch; a zero-affected update means a newer judgement owns the rows and the
//!   driver aborts with [`SdkError::StaleEpoch`] rather than clobbering it.
//! - **Bounded round-tripped state.** The whole judging state is re-serialized
//!   across the WASM host boundary on *every* result callback. Per-case
//!   `stdout`/`stderr` are persisted to the DB when the row is written and then
//!   *stripped* from the retained state - keeping them would make the
//!   round-tripped payload grow with each recorded case (an O(n^2) blow-up in
//!   output bytes). The final verdict only needs the per-case verdict and score.
//! - **Idempotent recording.** A result for an already-recorded case (a late
//!   verdict for a case the policy already cancelled, say) is ignored, never
//!   double-inserted.
//! - **Terminal always reached.** Compile error, short-circuit, timeout, and a
//!   mid-stream persistence failure all funnel to a filled-in terminal verdict,
//!   so a submission can never be stranded on "Judging".

use std::collections::HashSet;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::Host;
use crate::error::SdkError;
use crate::types::{
    DetachedEvaluateCallbackEvent, DetachedEvaluateCallbackInput, DetachedEvaluateCallbackOutput,
    OnSubmissionInput, SourceFile, StartEvaluateBatchInput, StartEvaluateCaseInput,
    TestCaseBodyRef, TestCaseResultRow, TestCaseRow, TestCaseVerdict, Verdict,
    default_evaluation_result_timeout_ms, sanitize_result_text_field,
};

/// Message stamped on cases skipped because an earlier case short-circuited
/// their subtask (or the whole batch).
pub const SKIPPED_SHORT_CIRCUIT: &str = "SKIPPED_SHORT_CIRCUIT";
/// Message stamped on cases filled in when evaluation times out or is exhausted.
pub const EVALUATION_TIMEOUT: &str = "EVALUATION_TIMEOUT";
/// Message stamped on every case when the batch could not even be started.
pub const BATCH_START_FAILED: &str = "BATCH_START_FAILED";
/// Message stamped on cases filled in when a mid-stream persistence call fails.
pub const EVALUATION_PERSIST_FAILED: &str = "EVALUATION_PERSIST_FAILED";

/// One finished test case, as scored by a [`ContestJudge`].
///
/// This is the single retained per-case record shared by every contest type.
/// `score` is the policy's own per-case number (ICPC: 1.0 for AC else 0.0; IOI:
/// the normalized 0..1 raw score) and is what [`ContestJudge::finalize`] reads
/// back to compute the submission verdict. `stdout`/`stderr` are populated only
/// transiently while the row is being written and are stripped from the copy the
/// driver retains (see the module docs).
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct CaseOutcome {
    pub test_case_id: i32,
    pub verdict: Verdict,
    pub score: f64,
    pub time_used: Option<i32>,
    pub memory_used: Option<i32>,
    pub message: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

impl CaseOutcome {
    /// Build an outcome from a host verdict, attaching the policy's per-case
    /// `score`. Owns the time/memory clamping so a policy never re-derives it.
    pub fn from_verdict(verdict: &TestCaseVerdict, score: f64) -> Self {
        Self {
            test_case_id: verdict.test_case_id,
            verdict: verdict.verdict.clone(),
            score,
            time_used: verdict.time_used_ms.map(clamp_i32),
            memory_used: verdict.memory_used_kb.map(clamp_i32),
            message: verdict.message.clone(),
            stdout: verdict.stdout.clone(),
            stderr: verdict.stderr.clone(),
        }
    }

    /// A synthetic (never-ran) outcome for a case the driver fills in itself:
    /// short-circuit `Skipped`, timeout `SystemError`, etc. Score is 0.
    fn filled(test_case_id: i32, verdict: Verdict, message: &str) -> Self {
        Self {
            test_case_id,
            verdict,
            score: 0.0,
            time_used: None,
            memory_used: None,
            message: Some(message.to_string()),
            stdout: None,
            stderr: None,
        }
    }
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(0, i32::MAX as i64) as i32
}

/// A read-only view of judging progress handed to a policy's decision hooks.
///
/// It exposes exactly what a contest type needs to decide the next step or the
/// final verdict, and nothing about the host protocol.
pub struct JudgeProgress<'a> {
    /// The submission being judged, WITHOUT its test cases or source files:
    /// the driver drops those after starting the batch (see
    /// [`without_bodies`]). Ids, limits, language and contest fields remain.
    pub request: &'a OnSubmissionInput,
    /// The cases the evaluate window draws from, with their `input` and
    /// `expected_output` bodies replaced by `Missing`. Everything a policy
    /// scores on (id, score, label, position, flags) is intact.
    pub scoring_cases: &'a [TestCaseRow],
    /// Every outcome recorded so far, in record order (output stripped).
    pub outcomes: &'a [CaseOutcome],
    /// The outcome that triggered this decision. `None` in `finalize` when the
    /// terminal was reached without a result (timeout / exhausted / start
    /// failure).
    pub last: Option<&'a CaseOutcome>,
    recorded_ids: &'a HashSet<i32>,
}

impl JudgeProgress<'_> {
    /// Whether every scoring case now has a recorded outcome.
    pub fn all_recorded(&self) -> bool {
        self.scoring_cases
            .iter()
            .all(|tc| self.recorded_ids.contains(&tc.id))
    }
}

/// What the driver should do after recording a case.
///
/// Deliberately declarative: the policy states intent, the driver translates it
/// into the host-protocol reply (including the `refill` hint and the terminal
/// persistence). A policy never constructs a [`DetachedEvaluateCallbackOutput`].
pub enum JudgeStep {
    /// Keep judging. The driver finishes automatically once all cases are
    /// recorded.
    Continue,
    /// Cancel these still-pending cases: record each as `Skipped` and tell the
    /// host to drop them from its queue, then keep judging the rest.
    Cancel(Vec<i32>),
    /// Stop now. Fill every still-unrecorded case with `fill` (verdict, message)
    /// if given, then finalize.
    Finish { fill: Option<(Verdict, String)> },
}

impl JudgeStep {
    /// Convenience: short-circuit the whole remaining batch to `Skipped`.
    pub fn short_circuit() -> Self {
        JudgeStep::Finish {
            fill: Some((Verdict::Skipped, SKIPPED_SHORT_CIRCUIT.to_string())),
        }
    }
}

/// A contest type's judging policy. Implementors express only *how a case scores*
/// and *when the batch stops*; the driver owns the rest (see module docs).
///
/// The policy value is itself part of the serialized session state, so it may
/// carry whatever it needs across callbacks (IOI keeps its subtask short-circuit
/// tracker and definitions here).
pub trait ContestJudge: Serialize + DeserializeOwned + Sized {
    /// Score one finished test case. Typically
    /// `CaseOutcome::from_verdict(result, my_score)`.
    fn score(&self, result: &TestCaseVerdict) -> CaseOutcome;

    /// The score written to the persisted test-case-result row. Defaults to the
    /// outcome's own per-case score; override to weight it by the case (IOI:
    /// `raw * tc.score`).
    fn db_score(&self, outcome: &CaseOutcome, _tc: &TestCaseRow) -> f64 {
        outcome.score
    }

    /// Decide how the batch proceeds after the driver records `progress.last`.
    fn next_step(&mut self, progress: &JudgeProgress<'_>) -> JudgeStep;

    /// Persist the terminal submission verdict/score from the recorded outcomes.
    fn finalize(&self, host: &Host, progress: &JudgeProgress<'_>) -> Result<(), SdkError>;
}

/// Driver-owned session state, serialized across the host boundary on every
/// callback. The contest type's [`ContestJudge`] rides along as `policy`.
#[derive(Serialize, serde::Deserialize)]
#[serde(bound(serialize = "J: Serialize", deserialize = "J: DeserializeOwned"))]
pub struct DetachedEval<J: ContestJudge> {
    request: OnSubmissionInput,
    scoring_cases: Vec<TestCaseRow>,
    outcomes: Vec<CaseOutcome>,
    recorded_ids: HashSet<i32>,
    marked_running: bool,
    policy: J,
}

impl<J: ContestJudge> DetachedEval<J> {
    /// Start a detached windowed evaluate for a submission. Call this from a
    /// contest type's `on_submission`. `callback_fn` is the plugin's exported
    /// result callback (which calls [`Self::handle_callback`]); `concurrency` is
    /// the evaluate window size.
    ///
    /// On success the host drives judging asynchronously and invokes
    /// `callback_fn` per result. If the batch cannot even be started, the whole
    /// submission is terminalized as `SystemError` here so it is never stranded.
    pub fn start(
        host: &Host,
        request: &OnSubmissionInput,
        scoring_cases: &[TestCaseRow],
        policy: J,
        callback_fn: &str,
        concurrency: usize,
    ) -> Result<(), SdkError> {
        let batch_input = build_eval_batch_input(request, scoring_cases);

        // A prior attempt on this (submission, judgement, epoch) may have left
        // rows; clear them so a retry starts clean. Best-effort: a stale epoch
        // no-ops the delete.
        let _ = host.submission.delete_results(
            request.submission_id,
            request.judgement_id,
            request.judge_epoch,
        );

        let affected = host.submission.set_compiling(
            request.submission_id,
            request.judgement_id,
            request.judge_epoch,
        )?;
        if affected == 0 {
            return Err(SdkError::StaleEpoch);
        }

        // The state is copied into the host and back on EVERY result
        // callback, so it must not carry the case bodies (or the source):
        // they are only needed to build `batch_input` above, which the host
        // keeps for dispatch and retries. Carrying them made each callback
        // parse and re-serialize the whole inline test data - measured at
        // ~0.33 s extra per case with 50 x 150 KB cases, and most of the
        // guest's ~20x memory amplification.
        let mut state = DetachedEval {
            request: request_without_bodies(request),
            scoring_cases: scoring_cases.iter().map(without_bodies).collect(),
            outcomes: Vec::new(),
            recorded_ids: HashSet::new(),
            marked_running: false,
            policy,
        };

        let start_result = host
            .eval
            .windowed(&batch_input)
            .concurrency(concurrency)
            .result_timeout_ms(default_evaluation_result_timeout_ms(request.time_limit_ms))
            .fire_after_judging_for_submission(request)
            .start_detached(callback_fn, serde_json::to_value(&state)?);

        if let Err(e) = start_result {
            state.fill_unrecorded(
                host,
                Verdict::SystemError,
                &format!("{BATCH_START_FAILED}: {e:?}"),
            )?;
            state.finalize(host)?;
        }

        Ok(())
    }

    /// Handle one detached evaluate callback. Call this from the plugin's
    /// exported result callback, passing the deserialized input; return its
    /// output to the host verbatim.
    pub fn handle_callback(
        host: &Host,
        input: DetachedEvaluateCallbackInput,
    ) -> Result<DetachedEvaluateCallbackOutput, SdkError> {
        let mut state: DetachedEval<J> = serde_json::from_value(input.state.clone())?;

        match &input.event {
            DetachedEvaluateCallbackEvent::Result { result } => state.on_result(host, result),
            DetachedEvaluateCallbackEvent::Timeout { .. }
            | DetachedEvaluateCallbackEvent::Exhausted => {
                state.fill_unrecorded(host, Verdict::SystemError, EVALUATION_TIMEOUT)?;
                state.finalize(host)?;
                Ok(DetachedEvaluateCallbackOutput::cancel(
                    serde_json::to_value(&state)?,
                ))
            }
        }
    }

    /// Best-effort recovery when a result callback fails mid-stream (a transient
    /// DB error, a rejected row). Fills any unrecorded case with `SystemError`
    /// and writes the terminal update, so the submission is not stranded on
    /// "Judging". All steps are best-effort: a stale epoch no-ops the writes and
    /// the worker's submission-level retry re-drives judging later.
    pub fn recover(host: &Host, state_value: &serde_json::Value) {
        let Ok(mut state) = serde_json::from_value::<DetachedEval<J>>(state_value.clone()) else {
            // Unparseable state - nothing actionable; the worker retry is the
            // remaining safety net.
            return;
        };
        let _ = state.fill_unrecorded(host, Verdict::SystemError, EVALUATION_PERSIST_FAILED);
        let _ = state.finalize(host);
    }

    fn on_result(
        &mut self,
        host: &Host,
        result: &TestCaseVerdict,
    ) -> Result<DetachedEvaluateCallbackOutput, SdkError> {
        // Idempotency: a result for an already-recorded case (a late verdict for
        // a case the policy cancelled, delivered after we recorded it Skipped) is
        // ignored, never double-inserted. Keep refilling while cases remain so an
        // unrelated pending subtask is never stranded.
        if self.recorded_ids.contains(&result.test_case_id) {
            let refill = self.any_unrecorded();
            return Ok(
                DetachedEvaluateCallbackOutput::continue_with(serde_json::to_value(&*self)?)
                    .refill(refill),
            );
        }

        let outcome = self.policy.score(result);

        // Compile error short-circuits the entire batch before the submission is
        // ever marked Running: record it, skip the rest, finalize.
        if outcome.verdict == Verdict::CompileError {
            self.record(host, outcome)?;
            self.fill_unrecorded(host, Verdict::Skipped, SKIPPED_SHORT_CIRCUIT)?;
            self.finalize(host)?;
            return Ok(DetachedEvaluateCallbackOutput::finish(
                serde_json::to_value(&*self)?,
            ));
        }

        // First real result flips the submission to Running (once).
        if !self.marked_running {
            let affected = host.submission.set_running(
                self.request.submission_id,
                self.request.judgement_id,
                self.request.judge_epoch,
            )?;
            if affected == 0 {
                return Err(SdkError::StaleEpoch);
            }
            self.marked_running = true;
        }

        self.record(host, outcome)?;

        let step = {
            let DetachedEval {
                request,
                scoring_cases,
                outcomes,
                recorded_ids,
                policy,
                ..
            } = self;
            let progress = JudgeProgress {
                request,
                scoring_cases,
                outcomes,
                last: outcomes.last(),
                recorded_ids,
            };
            policy.next_step(&progress)
        };

        match step {
            JudgeStep::Continue => self.continue_or_finish(host, Vec::new()),
            JudgeStep::Cancel(ids) => {
                for id in &ids {
                    self.record(
                        host,
                        CaseOutcome::filled(*id, Verdict::Skipped, SKIPPED_SHORT_CIRCUIT),
                    )?;
                }
                self.continue_or_finish(host, ids)
            }
            JudgeStep::Finish { fill } => {
                if let Some((verdict, message)) = fill {
                    self.fill_unrecorded(host, verdict, &message)?;
                }
                self.finalize(host)?;
                Ok(DetachedEvaluateCallbackOutput::finish(
                    serde_json::to_value(&*self)?,
                ))
            }
        }
    }

    /// After a `Continue`/`Cancel` step: finish if everything is recorded,
    /// otherwise reply `Continue` with the driver-computed refill hint and any
    /// cancellations.
    fn continue_or_finish(
        &mut self,
        host: &Host,
        cancel_ids: Vec<i32>,
    ) -> Result<DetachedEvaluateCallbackOutput, SdkError> {
        if self.all_recorded() {
            self.finalize(host)?;
            return Ok(DetachedEvaluateCallbackOutput::finish(
                serde_json::to_value(&*self)?,
            ));
        }
        let refill = self.any_unrecorded();
        Ok(
            DetachedEvaluateCallbackOutput::continue_with(serde_json::to_value(&*self)?)
                .refill(refill)
                .cancel_test_case_ids(cancel_ids),
        )
    }

    /// Insert the row for `outcome` (scored via the policy), mark it recorded,
    /// and retain a stripped copy (no stdout/stderr) in the session state.
    fn record(&mut self, host: &Host, outcome: CaseOutcome) -> Result<(), SdkError> {
        let tc = self
            .scoring_cases
            .iter()
            .find(|tc| tc.id == outcome.test_case_id);
        let score = match tc {
            Some(tc) => self.policy.db_score(&outcome, tc),
            None => 0.0,
        };
        let row = build_tc_row(&self.request, &outcome, score, tc);
        host.submission.insert_results(&[row])?;

        self.recorded_ids.insert(outcome.test_case_id);
        self.outcomes.push(CaseOutcome {
            stdout: None,
            stderr: None,
            ..outcome
        });
        Ok(())
    }

    /// Record a filled-in outcome for every scoring case not yet recorded.
    fn fill_unrecorded(
        &mut self,
        host: &Host,
        verdict: Verdict,
        message: &str,
    ) -> Result<(), SdkError> {
        let unrecorded: Vec<i32> = self
            .scoring_cases
            .iter()
            .map(|tc| tc.id)
            .filter(|id| !self.recorded_ids.contains(id))
            .collect();
        for id in unrecorded {
            self.record(host, CaseOutcome::filled(id, verdict.clone(), message))?;
        }
        Ok(())
    }

    fn finalize(&self, host: &Host) -> Result<(), SdkError> {
        let progress = JudgeProgress {
            request: &self.request,
            scoring_cases: &self.scoring_cases,
            outcomes: &self.outcomes,
            last: None,
            recorded_ids: &self.recorded_ids,
        };
        self.policy.finalize(host, &progress)
    }

    fn all_recorded(&self) -> bool {
        self.scoring_cases
            .iter()
            .all(|tc| self.recorded_ids.contains(&tc.id))
    }

    fn any_unrecorded(&self) -> bool {
        self.scoring_cases
            .iter()
            .any(|tc| !self.recorded_ids.contains(&tc.id))
    }
}

/// Build the host evaluate-batch input for a submission's cases. Uniform across
/// contest types; exposed so plugins share one definition.
/// A copy of `tc` with its `input` and `expected_output` bodies dropped
/// (`Missing`), for anything kept past batch start: session state and
/// scoring policies need a case's identity and weight, never its data.
///
/// Built field by field rather than `..tc.clone()`, which would copy the
/// bodies only to drop them.
pub fn without_bodies(tc: &TestCaseRow) -> TestCaseRow {
    TestCaseRow {
        id: tc.id,
        score: tc.score,
        is_sample: tc.is_sample,
        position: tc.position,
        description: tc.description.clone(),
        label: tc.label.clone(),
        input: TestCaseBodyRef::Missing,
        expected_output: TestCaseBodyRef::Missing,
        is_custom: tc.is_custom,
    }
}

/// `req` minus its test cases and source files, for the session state. Every
/// field is listed on purpose: adding one to `OnSubmissionInput` fails to
/// compile here until someone decides whether it belongs in state that is
/// copied on every callback.
fn request_without_bodies(req: &OnSubmissionInput) -> OnSubmissionInput {
    OnSubmissionInput {
        submission_id: req.submission_id,
        judgement_id: req.judgement_id,
        fire_after_judging: req.fire_after_judging,
        user_id: req.user_id,
        problem_id: req.problem_id,
        contest_id: req.contest_id,
        files: Vec::new(),
        language: req.language.clone(),
        time_limit_ms: req.time_limit_ms,
        memory_limit_kb: req.memory_limit_kb,
        problem_type: req.problem_type.clone(),
        test_cases: Vec::new(),
        judge_epoch: req.judge_epoch,
        target_worker_id: req.target_worker_id.clone(),
    }
}

pub fn build_eval_batch_input(
    req: &OnSubmissionInput,
    test_cases: &[TestCaseRow],
) -> StartEvaluateBatchInput {
    StartEvaluateBatchInput {
        problem_type: req.problem_type.clone(),
        test_cases: test_cases
            .iter()
            .map(|tc| StartEvaluateCaseInput {
                problem_id: req.problem_id,
                test_case_id: tc.id,
                solution_source: req
                    .files
                    .iter()
                    .map(|f| SourceFile {
                        filename: f.filename.clone(),
                        content: f.content.clone(),
                    })
                    .collect(),
                solution_language: req.language.clone(),
                time_limit_ms: req.time_limit_ms,
                memory_limit_kb: req.memory_limit_kb,
                contest_id: req.contest_id,
                input: tc.input.clone(),
                expected_output: tc.expected_output.clone(),
                is_custom: tc.is_custom,
                target_worker_id: req.target_worker_id.clone(),
            })
            .collect(),
    }
}

/// Build a persisted test-case-result row. Uniform across contest types: the
/// only policy input is the already-computed `score`. Custom (user-submitted)
/// cases persist under `run_index` rather than `test_case_id`.
fn build_tc_row(
    req: &OnSubmissionInput,
    outcome: &CaseOutcome,
    score: f64,
    tc: Option<&TestCaseRow>,
) -> TestCaseResultRow {
    let is_custom = tc.is_some_and(|t| t.is_custom);
    let (test_case_id, run_index) = if is_custom {
        (None, Some(outcome.test_case_id))
    } else {
        (Some(outcome.test_case_id), None)
    };
    TestCaseResultRow {
        submission_id: req.submission_id,
        judgement_id: req.judgement_id,
        judge_epoch: req.judge_epoch,
        test_case_id,
        run_index,
        verdict: outcome.verdict.clone(),
        score,
        time_used: outcome.time_used,
        memory_used: outcome.memory_used,
        message: sanitize_optional_text(outcome.message.as_deref()),
        stdout: sanitize_optional_text(outcome.stdout.as_deref()),
        stderr: sanitize_optional_text(outcome.stderr.as_deref()),
    }
}

fn sanitize_optional_text(value: Option<&str>) -> Option<String> {
    value.map(|s| sanitize_result_text_field(s).into_owned())
}
