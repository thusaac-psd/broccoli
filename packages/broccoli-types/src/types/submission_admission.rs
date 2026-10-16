use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Atomic admission effects supplied by the contest owner. Values and metadata
/// are opaque to the host. Observe the slot before reading policy inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionAdmission {
    /// Policy state that must remain unchanged until admission commits.
    pub guard: SubmissionStorageGuard,
    /// Plugin-defined group for querying the admitted submissions later.
    pub feedback_key: String,
    /// Returned verbatim to the owner; the host does not interpret it.
    pub metadata: Value,
    /// Optional exclusive slot, held until feedback or the slot is released.
    pub slot: Option<SubmissionSlotToken>,
    /// Exclusive host admission cutoff, Unix epoch milliseconds. None has no
    /// cutoff. This is independent of any plugin scoring metadata.
    pub latest_arrival_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionStorageGuard {
    key: String,
    expected_value: String,
}

impl SubmissionStorageGuard {
    /// Guard the observed document value. Admission itself advances the host
    /// revision without changing this value, so independent slots can admit
    /// against the same state. State writes use revision-aware storage CAS.
    pub fn from_snapshot(key: impl Into<String>, snapshot: StorageSnapshot) -> Self {
        Self {
            key: key.into(),
            expected_value: snapshot.value,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn expected_value(&self) -> &str {
        &self.expected_value
    }
}

/// An observation of an exclusive slot, checked again at admission. This is a
/// concurrency token, not an authorization credential; the host scopes it to
/// the calling plugin. Obtain it with `host.submission.observe_slot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionSlotToken {
    key: String,
    generation: Option<i32>,
}

impl SubmissionSlotToken {
    /// Used by the host and native SDK mock to construct an observation.
    pub fn new(key: String, generation: Option<i32>) -> Self {
        Self { key, generation }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn generation(&self) -> Option<i32> {
        self.generation
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionSlotObservation {
    pub token: SubmissionSlotToken,
    pub pending: Option<SubmissionFeedbackRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmissionFeedbackResult {
    pub verdict: super::Verdict,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "result", rename_all = "snake_case")]
pub enum ReleaseResult {
    Original,
    Override { verdict: super::Verdict, score: f64 },
}

impl ReleaseResult {
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Self::Override { score, .. } = self
            && !score.is_finite()
        {
            return Err("Invalid feedback score");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", content = "result", rename_all = "snake_case")]
pub enum SubmissionFeedbackAction {
    Release(ReleaseResult),
    ReleaseSlot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedbackUpdateOutcome {
    Applied,
    AlreadyApplied,
}

pub const FEEDBACK_PAGE_SIZE: u32 = 100;
pub const FEEDBACK_MAX_PAGE_SIZE: u32 = 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SubmissionFeedbackQuery {
    pub key: Option<String>,
    pub slot_key: Option<String>,
    pub submission_ids: Option<Vec<i32>>,
    pub unpublished_only: bool,
    pub after_id: i32,
    /// Page size, clamped to 1..=1000. Use `feedback_all` to read all pages.
    pub limit: u32,
}

impl Default for SubmissionFeedbackQuery {
    fn default() -> Self {
        Self {
            key: None,
            slot_key: None,
            submission_ids: None,
            unpublished_only: false,
            after_id: 0,
            limit: FEEDBACK_PAGE_SIZE,
        }
    }
}

impl SubmissionFeedbackQuery {
    pub fn page_size(&self) -> u32 {
        self.limit.clamp(1, FEEDBACK_MAX_PAGE_SIZE)
    }
}

/// Full lifecycle read from submissions. Judge writes continue to use the
/// narrower `SubmissionStatus` in `SubmissionUpdate`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubmissionLifecycle {
    Queued,
    #[default]
    Pending,
    Compiling,
    Running,
    Judged,
    CompilationError,
    SystemError,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubmissionFeedbackRecord {
    pub submission_id: i32,
    pub contest_id: i32,
    pub user_id: i32,
    pub problem_id: i32,
    pub submitted_at_us: i64,
    pub status: SubmissionLifecycle,
    pub verdict: Option<super::Verdict>,
    pub key: String,
    pub metadata: Value,
    pub withheld: bool,
    pub slot_held: bool,
    pub slot_key: Option<String>,
    pub result: Option<SubmissionFeedbackResult>,
}

impl SubmissionFeedbackRecord {
    /// Shared transition rules for the host and native SDK mock. Releases are
    /// final: identical retries succeed, but a different published result is a
    /// conflict. Releasing a slot never makes a hidden result public.
    pub fn apply_action(
        &mut self,
        action: &SubmissionFeedbackAction,
    ) -> Result<FeedbackUpdateOutcome, &'static str> {
        match action {
            SubmissionFeedbackAction::ReleaseSlot => {
                if !self.slot_held {
                    return Ok(FeedbackUpdateOutcome::AlreadyApplied);
                }
                self.slot_held = false;
            }
            SubmissionFeedbackAction::Release(release) => {
                release.validate()?;
                let result = match release {
                    ReleaseResult::Original => None,
                    ReleaseResult::Override { verdict, score } => Some(SubmissionFeedbackResult {
                        verdict: verdict.clone(),
                        score: *score,
                    }),
                };
                if !self.withheld {
                    return if self.result == result {
                        Ok(FeedbackUpdateOutcome::AlreadyApplied)
                    } else {
                        Err("Feedback was already released with a different result")
                    };
                }
                self.withheld = false;
                self.slot_held = false;
                self.result = result;
            }
        }
        Ok(FeedbackUpdateOutcome::Applied)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageSnapshot {
    pub value: String,
    pub revision: i64,
}
