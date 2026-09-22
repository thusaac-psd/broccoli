//! Typed storage schema for the 下午场 bracket plugin.

use serde::{Deserialize, Serialize};

/// Default grace period (seconds) between a match entering
/// `MatchPhase::AwaitingJudge` and its escalation timer firing. See
/// [`Setup::escalation_grace_seconds`].
pub fn default_escalation_grace_seconds() -> i64 {
    120
}

/// Round-level problem assignment and timing, submitted once via `/setup`
/// before the bracket begins. Stored under [`crate::storage::setup_key`].
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Setup {
    pub rounds: Vec<RoundDef>,
    pub xiaoju_seconds: i64,
    pub round_intermission_seconds: i64,
    /// How long a match may sit in `MatchPhase::AwaitingJudge` (blocked on
    /// an in-flight submission past its 小局 deadline) before escalating to
    /// `MatchPhase::NeedsAdjudication` for staff. `#[serde(default)]` so
    /// both a persisted `Setup` document written before this field existed,
    /// and a `/setup` request body that omits it, fall back to
    /// [`default_escalation_grace_seconds`] rather than failing to
    /// deserialize or silently becoming `0`.
    #[serde(default = "default_escalation_grace_seconds")]
    pub escalation_grace_seconds: i64,
}

/// One round's problem split: two groups of 3 (one per player of a match),
/// plus an ORDERED list of 附加赛 (tiebreak) problems.
///
/// `tiebreak` is a list, not a single problem, because a scoreless 附加赛
/// repeats with a NEW problem rather than the same one again: "附加赛中，
/// 双方将面对相同的题目，并根据该局的比赛结果决出最终胜者" says nothing
/// about a scoreless outcome, and the design decision taken is to advance to
/// the next tiebreak problem in this list rather than replay the same one.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RoundDef {
    pub group_a: [i32; 3],
    pub group_b: [i32; 3],
    pub tiebreak: Vec<i32>,
}

/// State of one bracket match between `player_a` and `player_b`.
///
/// # `order_a` / `order_b` name the order IMPOSED on that player
///
/// This is the single most likely bug in this plugin, so it is stated
/// plainly here and at every call site that touches these fields: each
/// player ranks the OPPONENT's problems, and that ranking becomes the order
/// the opponent must solve in. So `order_a` is NOT "the order player A
/// submitted" -- it is "the order player A must follow", which was
/// submitted BY player B. Symmetrically, `order_b` was submitted by player
/// A. A test that only exercises the symmetric case (both players ranking
/// simultaneously) cannot catch the fields being swapped; see
/// `ordering::player_b_submitting_sets_the_order_imposed_on_player_a` for
/// the test that asserts the direction explicitly.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct MatchState {
    pub round: u8,
    pub pos: u8,
    pub player_a: i32,
    pub player_b: i32,
    /// Player A's own 3 problems for this round (from `RoundDef::group_a`).
    /// Player B ranks THESE to produce `order_a`.
    pub group_a: [i32; 3],
    /// Player B's own 3 problems for this round (from `RoundDef::group_b`).
    /// Player A ranks THESE to produce `order_b`.
    pub group_b: [i32; 3],
    /// The order player A must solve THEIR OWN group's problems in, as
    /// ranked by player B (A's opponent). `None` until B submits a ranking.
    pub order_a: Option<[i32; 3]>,
    /// The order player B must solve THEIR OWN group's problems in, as
    /// ranked by player A (B's opponent). `None` until A submits a ranking.
    pub order_b: Option<[i32; 3]>,
    pub xiaoju: Vec<XiaojuState>,
    pub score_a: u8,
    pub score_b: u8,
    pub state: MatchPhase,
    pub winner: Option<i32>,
    /// Index into this round's `RoundDef::tiebreak` list of the 附加赛
    /// problem currently (or most recently) in play.
    pub tiebreak_index: usize,
    /// When this match reached `MatchPhase::Decided` (Unix epoch
    /// milliseconds), `0` until then. Needed to compute a ROUND-WIDE
    /// intermission boundary ("Intermission | Fixed, between rounds only"):
    /// the next round may not open before the fixed intermission has
    /// elapsed after the LAST match of the previous round finished, which
    /// requires knowing when each match finished, not just that it did.
    /// See `bracket::round_ended_at_ms`.
    pub decided_at_ms: i64,
    /// While `state == MatchPhase::AwaitingJudge`: the id of the in-flight
    /// submission this match is blocked on, so staff reading
    /// `GET /matches/{id}` know which submission to rejudge. `None`
    /// otherwise. See `MatchPhase::AwaitingJudge`'s doc comment for the
    /// policy this exists to support.
    pub awaiting_submission_id: Option<i32>,
}

/// State of one 小局 (the best-of-one sub-match on a single problem).
///
/// `winner: Option<i32>` here is deliberately a different shape from
/// `MatchOutcome::Decided { winner: i32 }`: a 小局 can legitimately end with
/// NEITHER player scoring ("如果在规定时间内双方均未能通过当前题目，则该
/// 小局双方均不得分"), which this type expresses as `Decided { winner: None
/// }` conceptually via `decided = true, winner = None`. A match, by
/// contrast, must always produce a winner or reach `NeedsAdjudication` --
/// never a scoreless terminal state. Do not "harmonise" these two shapes.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct XiaojuState {
    pub index: u8,
    pub opened_at_ms: i64,
    pub deadline_ms: i64,
    pub winner: Option<i32>,
    pub decided: bool,
}

/// A match's lifecycle phase.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatchPhase {
    /// Slots filled (or not yet); ordering has not started.
    #[default]
    Pending,
    /// Both players may read each other's group and submit a ranking.
    Ordering,
    /// The 3 regular 小局 are running.
    InProgress,
    /// Scores were level after 3 小局; an 附加赛 is running.
    Tiebreak,
    /// A 小局's deadline passed while an older submission was still IN
    /// FLIGHT (queued, pending, compiling, running, or a `SystemError`
    /// being retried -- see `decide::is_in_flight`) and could still beat
    /// the current best AC once judged. Awarding the AC now could hand the
    /// 小局 to the wrong player; staying in `InProgress`/`Tiebreak` forever
    /// would let a platform fault cost a player the 小局 outright with no
    /// automatic recovery. The contest owner's chosen policy: never let a
    /// platform fault decide it silently -- surface it as a distinct,
    /// staff-visible state (`awaiting_submission_id` names the blocking
    /// submission) with an escalation timer
    /// (`Setup::escalation_grace_seconds`) to `NeedsAdjudication` if the
    /// block outlives its grace period. If the blocking submission's
    /// verdict lands first, the match resumes normally -- and if it turns
    /// out accepted, the EARLIER submitter wins, per the earliest-submitted
    /// rule this whole plugin is built around.
    AwaitingJudge,
    /// The match has a winner.
    Decided,
    /// The tiebreak problem list was exhausted without a decision; staff
    /// must resolve this match manually.
    NeedsAdjudication,
}
