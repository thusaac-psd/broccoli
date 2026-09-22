//! Typed storage schema for the 下午场 bracket plugin.

use serde::{Deserialize, Serialize};

/// Round-level problem assignment and timing, submitted once via `/setup`
/// before the bracket begins. Stored under [`crate::storage::setup_key`].
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Setup {
    pub rounds: Vec<RoundDef>,
    pub xiaoju_seconds: i64,
    pub round_intermission_seconds: i64,
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
    /// The match has a winner.
    Decided,
    /// The tiebreak problem list was exhausted without a decision; staff
    /// must resolve this match manually.
    NeedsAdjudication,
}
