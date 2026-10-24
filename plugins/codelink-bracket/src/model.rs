//! Typed storage schema for the 下午场 bracket plugin.

use serde::{Deserialize, Serialize};

/// Default grace period (seconds) between a match entering
/// `MatchPhase::AwaitingJudge` and its escalation timer firing. See
/// [`Setup::escalation_grace_seconds`].
pub fn default_escalation_grace_seconds() -> i64 {
    120
}

pub fn default_penalty_seconds() -> i64 {
    20
}

/// Bracket seeding and stage rules, stored under [`crate::storage::setup_key`].
/// Unopened stages remain editable after the bracket begins.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Setup {
    pub rounds: Vec<RoundDef>,
    pub seeds: Vec<i32>,
    /// Prefix of stages whose rules are frozen, including interrupted openings.
    pub opened_rounds: usize,
    /// Shared by both players across this match's regular games only.
    #[serde(default)]
    pub match_extra_time_seconds: i64,
    /// Score penalty added to subsequent submissions for each WA/TLE/MLE/RE
    /// in a regular game, without shortening the submission window or spending
    /// extra time. Tiebreaks never apply penalties. Zero disables penalties;
    /// older setups retain the 20s default.
    #[serde(default = "default_penalty_seconds")]
    pub penalty_seconds: i64,
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

impl Default for Setup {
    fn default() -> Self {
        Self {
            rounds: Vec::new(),
            seeds: Vec::new(),
            opened_rounds: 0,
            match_extra_time_seconds: 0,
            penalty_seconds: default_penalty_seconds(),
            escalation_grace_seconds: default_escalation_grace_seconds(),
        }
    }
}

/// One round's problem split: two equally sized groups (one per player of a match),
/// plus an ORDERED list of 附加赛 (tiebreak) problems.
///
/// `tiebreak` is a list, not a single problem, because a scoreless 附加赛
/// repeats with a NEW problem rather than the same one again: "附加赛中，
/// 双方将面对相同的题目，并根据该局的比赛结果决出最终胜者" says nothing
/// about a scoreless outcome, and the design decision taken is to advance to
/// the next tiebreak problem in this list rather than replay the same one.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RoundDef {
    pub group_a: Vec<i32>,
    pub group_b: Vec<i32>,
    pub tiebreak: Vec<i32>,
    /// First to this many regular-game wins (FT). Group length is the game
    /// cap (BO). Omission uses a majority of the configured regular games.
    #[serde(default)]
    pub wins_to_advance: Option<usize>,
    pub xiaoju_seconds: i64,
    pub tiebreak_seconds: i64,
    /// Wait after the last match of the preceding stage. Zero in round one.
    pub break_before_seconds: i64,
}

/// Durations pinned when a match starts, including its later tiebreaks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameDurations {
    pub xiaoju_seconds: i64,
    pub tiebreak_seconds: i64,
}

impl Setup {
    pub fn player_count(&self) -> u8 {
        self.seeds.len() as u8
    }
}

impl MatchState {
    pub fn is_regular_game(&self, index: u8) -> bool {
        (index as usize) < self.group_a.len()
    }
}

impl RoundDef {
    pub fn win_target(&self) -> usize {
        self.wins_to_advance.unwrap_or(self.group_a.len() / 2 + 1)
    }

    pub fn game_durations(&self) -> GameDurations {
        GameDurations {
            xiaoju_seconds: self.xiaoju_seconds,
            tiebreak_seconds: self.tiebreak_seconds,
        }
    }
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
    /// Player A's own problems for this round (from `RoundDef::group_a`).
    /// Player B ranks THESE to produce `order_a`.
    pub group_a: Vec<i32>,
    /// Player B's own problems for this round (from `RoundDef::group_b`).
    /// Player A ranks THESE to produce `order_b`.
    pub group_b: Vec<i32>,
    /// The order player A must solve THEIR OWN group's problems in, as
    /// ranked by player B (A's opponent). `None` until B submits a ranking.
    pub order_a: Option<Vec<i32>>,
    /// The order player B must solve THEIR OWN group's problems in, as
    /// ranked by player A (B's opponent). `None` until A submits a ranking.
    pub order_b: Option<Vec<i32>>,
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
    /// Read by `bracket::player_free_at_ms` to time the winner's break.
    pub decided_at_ms: i64,
    /// While `state == MatchPhase::AwaitingJudge`: the id of the in-flight
    /// submission this match is blocked on, so staff reading
    /// `GET /matches/{id}` know which submission to rejudge. `None`
    /// otherwise. See `MatchPhase::AwaitingJudge`'s doc comment for the
    /// policy this exists to support.
    pub awaiting_submission_id: Option<i32>,
    /// Why the match is in `NeedsAdjudication`, so staff (and the UI) are
    /// told the actual cause instead of guessing it from other fields.
    /// `None` in any other phase, and for documents written before this
    /// field existed (`#[serde(default)]`).
    #[serde(default)]
    pub adjudication_reason: Option<AdjudicationReason>,
    /// None until the first game opens. Both durations then remain fixed for
    /// this match, even if staff correct the stage setup during play.
    pub game_durations: Option<GameDurations>,
    /// Pinned at the first game along with the base duration. Legacy matches
    /// that already opened a game keep a zero allowance.
    #[serde(default)]
    pub match_extra_time_seconds: i64,
    /// Balance after settled games. While a game is open, its deadlines
    /// describe the running clock; only settlement changes this balance.
    #[serde(default)]
    pub extra_time_remaining_ms: i64,
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
    /// Missing in old documents, whose final deadline is also their base deadline.
    #[serde(default)]
    pub base_deadline_ms: Option<i64>,
    pub deadline_ms: i64,
    #[serde(default)]
    pub extra_time_used_ms: i64,
    pub winner: Option<i32>,
    pub decided: bool,
}

impl XiaojuState {
    pub fn base_deadline_ms(&self) -> i64 {
        self.base_deadline_ms.unwrap_or(self.deadline_ms)
    }

    /// The same half-open window is enforced at submission and settlement.
    /// Use i128 for the comparison so even an unbounded test deadline is safe.
    pub fn contains_submission_at_us(&self, submitted_at_us: i64) -> bool {
        let at = i128::from(submitted_at_us);
        i128::from(self.opened_at_ms) * 1_000 <= at && at < i128::from(self.deadline_ms) * 1_000
    }
}

/// Why a match needs a staff decision.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdjudicationReason {
    /// A submission was still being judged when the escalation grace ran
    /// out (`awaiting_submission_id` names it).
    StuckJudge,
    /// Every tiebreak problem ended scoreless.
    TiebreakExhausted,
    /// The round has no problem definition to continue with.
    SetupMissing,
    /// The contest's end time arrived before the match could finish: nobody
    /// can submit any more, so the remaining games cannot be played.
    ContestEnded,
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
    /// The regular 小局 are running.
    InProgress,
    /// Scores were level after the regular 小局; an 附加赛 is running.
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
