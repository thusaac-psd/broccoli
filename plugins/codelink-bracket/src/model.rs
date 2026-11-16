//! Typed storage schema for the 下午场 bracket plugin.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Default grace period (seconds) between a match entering
/// `Phase::AwaitingJudge` and its escalation to staff. See
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
    /// Optional bronze match, played alongside the final using the final
    /// stage's timing and BO/FT settings, with its own problem groups.
    pub third_place: Option<ThirdPlaceConfig>,
    /// Prefix of stages whose rules are frozen, including interrupted openings.
    pub opened_rounds: usize,
    pub stage_clocks: BTreeMap<u8, StageClock>,
    /// Shared by both players across this match's regular games only.
    pub match_extra_time_seconds: i64,
    /// Score penalty added to subsequent submissions for each WA/TLE/MLE/RE
    /// in a regular game, without shortening the submission window or spending
    /// extra time. Tiebreaks never apply penalties. Zero disables penalties.
    pub penalty_seconds: i64,
    /// How long a match may sit in `Phase::AwaitingJudge` (blocked on an
    /// in-flight submission past its 小局 deadline) before escalating to
    /// `Phase::NeedsAdjudication` for staff.
    pub escalation_grace_seconds: i64,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            rounds: Vec::new(),
            seeds: Vec::new(),
            third_place: None,
            opened_rounds: 0,
            stage_clocks: BTreeMap::new(),
            match_extra_time_seconds: 0,
            penalty_seconds: default_penalty_seconds(),
            escalation_grace_seconds: default_escalation_grace_seconds(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThirdPlaceProblems {
    pub group_a: Vec<i32>,
    pub group_b: Vec<i32>,
    pub tiebreak: Vec<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ThirdPlaceConfig {
    Shared,
    Custom(ThirdPlaceProblems),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    Elimination,
    Final,
    ThirdPlace,
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
    /// UTC time at which the whole stage starts its 180-second ordering phase.
    /// Without one, the stage opens once the previous stage and its break are
    /// complete, and each match starts once both rankings arrive.
    #[serde(default)]
    pub start_at_ms: Option<i64>,
    #[serde(default)]
    pub post_order_wait_seconds: Option<i64>,
    #[serde(default)]
    pub game_break_seconds: Option<i64>,
    #[serde(default)]
    pub match_extra_time_seconds: Option<i64>,
    pub xiaoju_seconds: i64,
    pub tiebreak_seconds: i64,
    /// Wait after the last match of the preceding stage. Zero in round one.
    pub break_before_seconds: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StageClock {
    pub start_at_ms: i64,
    pub ordering_deadline_ms: i64,
    pub starts_at_ms: i64,
    pub awaiting_reschedule: bool,
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

    pub fn match_count(&self, round: u8) -> u8 {
        crate::storage::matches_in_round(self.player_count(), round)
            + u8::from(self.third_place.is_some() && usize::from(round) == self.rounds.len())
    }

    pub fn total_matches(&self) -> usize {
        self.seeds.len().saturating_sub(1) + usize::from(self.third_place.is_some())
    }

    pub fn match_kind(&self, round: u8, pos: u8) -> MatchKind {
        if usize::from(round) != self.rounds.len() {
            MatchKind::Elimination
        } else if pos == 1 && self.third_place.is_some() {
            MatchKind::ThirdPlace
        } else {
            MatchKind::Final
        }
    }

    pub fn round_for_match(&self, round: u8, pos: u8) -> Option<Cow<'_, RoundDef>> {
        let base = self.rounds.get(usize::from(round.checked_sub(1)?))?;
        if pos >= self.match_count(round) {
            return None;
        }
        if self.match_kind(round, pos) == MatchKind::ThirdPlace {
            let ThirdPlaceConfig::Custom(problems) = self.third_place.as_ref()? else {
                return Some(Cow::Borrowed(base));
            };
            let mut resolved = base.clone();
            resolved.group_a = problems.group_a.clone();
            resolved.group_b = problems.group_b.clone();
            resolved.tiebreak = problems.tiebreak.clone();
            Some(Cow::Owned(resolved))
        } else {
            Some(Cow::Borrowed(base))
        }
    }

    /// Whether `round` follows a shared stage schedule (see
    /// `RoundDef::start_at_ms`).
    pub fn is_scheduled(&self, round: u8) -> bool {
        round
            .checked_sub(1)
            .and_then(|index| self.rounds.get(usize::from(index)))
            .is_some_and(|r| r.start_at_ms.is_some())
    }

    pub fn problem_round(&self, problem: i32) -> Option<u8> {
        self.rounds
            .iter()
            .position(|r| r.contains_problem(problem))
            .map(|index| index as u8 + 1)
            .or_else(|| match self.third_place.as_ref()? {
                ThirdPlaceConfig::Custom(p)
                    if p.group_a.contains(&problem)
                        || p.group_b.contains(&problem)
                        || p.tiebreak.contains(&problem) =>
                {
                    Some(self.rounds.len() as u8)
                }
                _ => None,
            })
    }
}

impl RoundDef {
    pub fn post_order_wait(&self) -> i64 {
        self.post_order_wait_seconds.unwrap_or(30)
    }

    pub fn game_break(&self) -> i64 {
        self.game_break_seconds
            .unwrap_or(if self.start_at_ms.is_some() { 60 } else { 0 })
    }
    pub fn contains_problem(&self, problem: i32) -> bool {
        self.group_a.contains(&problem)
            || self.group_b.contains(&problem)
            || self.tiebreak.contains(&problem)
    }

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
    /// Every game opened so far, regular games first, then tiebreak
    /// attempts. A game's index alone says which kind it is (see
    /// [`MatchState::game_kind`]).
    pub xiaoju: Vec<XiaojuState>,
    pub score_a: u8,
    pub score_b: u8,
    /// Changed only by [`crate::machine::transition`].
    pub phase: Phase,
    /// None until the first game opens. Both durations then remain fixed for
    /// this match, even if staff correct the stage setup during play.
    pub game_durations: Option<GameDurations>,
    /// Pinned at the first game along with the base duration.
    pub match_extra_time_seconds: i64,
    /// Balance after settled games. While a game is open, its deadlines
    /// describe the running clock; only settlement changes this balance.
    pub extra_time_remaining_ms: i64,
}

/// Where a match is in its lifecycle. Each phase carries exactly the data
/// that exists while the match is in it, so a winner without a decision, or
/// a blocking submission outside `AwaitingJudge`, cannot be represented.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Phase {
    /// Both players may read each other's group and submit a ranking.
    #[default]
    Ordering,
    /// Ordering has closed; the whole stage waits for its common game start.
    Preparing,
    /// The last game in `xiaoju` is open for submissions. Whether it is a
    /// regular game or a tiebreak follows from its index.
    Playing,
    /// A settled game is followed by a break before the next game opens.
    Intermission { next_game_at_ms: i64 },
    /// The open game's deadline passed while an older submission was still
    /// in flight (see `decide::submission_in_flight`) and could still beat
    /// the best accepted one. Awarding the game now could hand it to the
    /// wrong player, and a platform fault must never decide it silently, so
    /// the match waits for that verdict until `escalate_at_ms`, then goes to
    /// staff. If the verdict lands first, the game is decided normally, and
    /// an accepted blocker wins because it was submitted earlier.
    AwaitingJudge {
        blocking_submission_id: i32,
        escalate_at_ms: i64,
    },
    /// The match has a winner.
    Decided { winner: i32, decided_at_ms: i64 },
    /// The match cannot continue on its own; staff must award it.
    NeedsAdjudication { adjudication: Adjudication },
}

impl Phase {
    /// Whether no further game can be played automatically. Admissions are
    /// retired and the match's timers are released once it is closed.
    pub fn is_closed(&self) -> bool {
        matches!(
            self,
            Phase::Decided { .. } | Phase::NeedsAdjudication { .. }
        )
    }
}

/// Why a match needs a staff decision, with the data each cause brings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Adjudication {
    /// `submission_id` was still being judged when the escalation grace ran
    /// out, so staff know which submission to rejudge.
    StuckJudge { submission_id: i32 },
    /// Every tiebreak problem ended scoreless.
    TiebreakExhausted,
    /// The round has no problem definition to continue with.
    SetupMissing,
    /// The contest's end time arrived before the match could finish: nobody
    /// can submit any more, so the remaining games cannot be played.
    ContestEnded,
}

impl Adjudication {
    pub fn reason(&self) -> AdjudicationReason {
        match self {
            Adjudication::StuckJudge { .. } => AdjudicationReason::StuckJudge,
            Adjudication::TiebreakExhausted => AdjudicationReason::TiebreakExhausted,
            Adjudication::SetupMissing => AdjudicationReason::SetupMissing,
            Adjudication::ContestEnded => AdjudicationReason::ContestEnded,
        }
    }
}

/// The wire name of an [`Adjudication`], as the HTTP views report it.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdjudicationReason {
    StuckJudge,
    TiebreakExhausted,
    SetupMissing,
    ContestEnded,
}

/// The wire name of a match's phase, as the HTTP views report it. A playing
/// match is reported as `in_progress` or `tiebreak` by its open game.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PhaseName {
    Ordering,
    Preparing,
    Intermission,
    InProgress,
    Tiebreak,
    AwaitingJudge,
    Decided,
    NeedsAdjudication,
}

/// Which kind of game an index in [`MatchState::xiaoju`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameKind {
    /// Each player solves their own problem at this index of the order
    /// imposed on them.
    Regular,
    /// Both players solve `RoundDef::tiebreak[attempt]`. A scoreless
    /// attempt moves on to the next problem rather than replaying this one.
    Tiebreak { attempt: usize },
}

impl MatchState {
    pub fn game_kind(&self, index: u8) -> GameKind {
        match usize::from(index).checked_sub(self.group_a.len()) {
            None => GameKind::Regular,
            Some(attempt) => GameKind::Tiebreak { attempt },
        }
    }

    pub fn is_regular_game(&self, index: u8) -> bool {
        self.game_kind(index) == GameKind::Regular
    }

    /// The game that is open or blocked on judging, if any.
    pub fn open_game(&self) -> Option<&XiaojuState> {
        match self.phase {
            Phase::Playing | Phase::AwaitingJudge { .. } => self.xiaoju.last(),
            _ => None,
        }
    }

    pub fn phase_name(&self) -> PhaseName {
        match self.phase {
            Phase::Ordering => PhaseName::Ordering,
            Phase::Preparing => PhaseName::Preparing,
            Phase::Intermission { .. } => PhaseName::Intermission,
            Phase::Playing => match self.xiaoju.last().map(|g| self.game_kind(g.index)) {
                Some(GameKind::Tiebreak { .. }) => PhaseName::Tiebreak,
                _ => PhaseName::InProgress,
            },
            Phase::AwaitingJudge { .. } => PhaseName::AwaitingJudge,
            Phase::Decided { .. } => PhaseName::Decided,
            Phase::NeedsAdjudication { .. } => PhaseName::NeedsAdjudication,
        }
    }

    pub fn winner(&self) -> Option<i32> {
        match self.phase {
            Phase::Decided { winner, .. } => Some(winner),
            _ => None,
        }
    }

    pub fn decided_at_ms(&self) -> Option<i64> {
        match self.phase {
            Phase::Decided { decided_at_ms, .. } => Some(decided_at_ms),
            _ => None,
        }
    }

    /// The submission staff should rejudge: the one the match is waiting
    /// for, or the one whose wait sent it to staff.
    pub fn blocking_submission_id(&self) -> Option<i32> {
        match self.phase {
            Phase::AwaitingJudge {
                blocking_submission_id,
                ..
            } => Some(blocking_submission_id),
            Phase::NeedsAdjudication {
                adjudication: Adjudication::StuckJudge { submission_id },
            } => Some(submission_id),
            _ => None,
        }
    }

    /// Index into `RoundDef::tiebreak` of the attempt that is in play, about
    /// to open after a break, or played last; `0` before any tiebreak.
    pub fn tiebreak_attempt(&self) -> usize {
        let index = match self.phase {
            Phase::Intermission { .. } => self.xiaoju.len(),
            _ => self.xiaoju.last().map_or(0, |g| usize::from(g.index)),
        };
        index.saturating_sub(self.group_a.len())
    }

    /// The problem `player` must currently submit to: their position in the
    /// order imposed on them in a regular game, or the shared problem of a
    /// tiebreak attempt. `None` outside an open game or for anyone else.
    /// Submission gating, judging and feedback all resolve the current
    /// problem here.
    pub fn current_problem(&self, round_def: &RoundDef, player: i32) -> Option<i32> {
        let game = self.open_game()?;
        match self.game_kind(game.index) {
            GameKind::Regular => {
                let order = if player == self.player_a {
                    self.order_a.as_ref()
                } else if player == self.player_b {
                    self.order_b.as_ref()
                } else {
                    return None;
                };
                order?.get(usize::from(game.index)).copied()
            }
            GameKind::Tiebreak { attempt } => (player == self.player_a || player == self.player_b)
                .then(|| round_def.tiebreak.get(attempt).copied())
                .flatten(),
        }
    }
}

/// State of one 小局 (the best-of-one sub-match on a single problem).
///
/// `winner: Option<i32>` here is deliberately a different shape from
/// `Phase::Decided { winner: i32 }`: a 小局 can legitimately end with
/// NEITHER player scoring ("如果在规定时间内双方均未能通过当前题目，则该
/// 小局双方均不得分"), which this type expresses as `decided = true, winner =
/// None`. A match, by contrast, must always produce a winner or reach
/// `NeedsAdjudication` -- never a scoreless terminal state. Do not
/// "harmonise" these two shapes.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct XiaojuState {
    pub index: u8,
    pub opened_at_ms: i64,
    /// The deadline without shared extra time.
    pub base_deadline_ms: i64,
    /// The deadline including the shared extra time still available when the
    /// game opened.
    pub deadline_ms: i64,
    pub extra_time_used_ms: i64,
    pub winner: Option<i32>,
    pub decided: bool,
}

impl XiaojuState {
    /// The same half-open window is enforced at submission and settlement.
    /// Use i128 for the comparison so even an unbounded test deadline is safe.
    pub fn contains_submission_at_us(&self, submitted_at_us: i64) -> bool {
        let at = i128::from(submitted_at_us);
        i128::from(self.opened_at_ms) * 1_000 <= at && at < i128::from(self.deadline_ms) * 1_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_def() -> RoundDef {
        RoundDef {
            tiebreak: vec![301, 302],
            ..Default::default()
        }
    }

    fn with_games(phase: Phase, games: u8) -> MatchState {
        MatchState {
            player_a: 10,
            player_b: 20,
            group_a: vec![101, 102, 103],
            group_b: vec![201, 202, 203],
            order_a: Some(vec![103, 101, 102]),
            order_b: Some(vec![203, 201, 202]),
            xiaoju: (0..games)
                .map(|index| XiaojuState {
                    index,
                    ..Default::default()
                })
                .collect(),
            phase,
            ..Default::default()
        }
    }

    fn blocked() -> Phase {
        Phase::AwaitingJudge {
            blocking_submission_id: 5,
            escalate_at_ms: 0,
        }
    }

    #[test]
    fn the_current_problem_follows_the_imposed_order_in_regular_games() {
        for phase in [Phase::Playing, blocked()] {
            let m = with_games(phase, 2);
            assert_eq!(m.current_problem(&round_def(), 10), Some(101));
            assert_eq!(m.current_problem(&round_def(), 20), Some(201));
            assert_eq!(m.current_problem(&round_def(), 99), None);
        }
    }

    #[test]
    fn both_players_share_the_current_tiebreak_problem_even_while_blocked() {
        for phase in [Phase::Playing, blocked()] {
            let m = with_games(phase, 5);
            assert_eq!(m.game_kind(4), GameKind::Tiebreak { attempt: 1 });
            assert_eq!(m.current_problem(&round_def(), 10), Some(302));
            assert_eq!(m.current_problem(&round_def(), 20), Some(302));
            assert_eq!(m.current_problem(&round_def(), 99), None);
        }
    }

    #[test]
    fn there_is_no_current_problem_without_an_open_game() {
        let pause = Phase::Intermission { next_game_at_ms: 0 };
        for phase in [Phase::Ordering, pause] {
            assert_eq!(with_games(phase, 1).current_problem(&round_def(), 10), None);
        }
    }

    #[test]
    fn a_playing_match_is_named_by_its_open_game() {
        assert_eq!(
            with_games(Phase::Playing, 1).phase_name(),
            PhaseName::InProgress
        );
        assert_eq!(
            with_games(Phase::Playing, 4).phase_name(),
            PhaseName::Tiebreak
        );
        assert_eq!(
            with_games(blocked(), 4).phase_name(),
            PhaseName::AwaitingJudge
        );
    }

    #[test]
    fn the_tiebreak_attempt_is_the_one_in_play_or_about_to_open() {
        assert_eq!(with_games(Phase::Ordering, 0).tiebreak_attempt(), 0);
        assert_eq!(with_games(Phase::Playing, 2).tiebreak_attempt(), 0);
        assert_eq!(with_games(Phase::Playing, 5).tiebreak_attempt(), 1);
        let pause = Phase::Intermission { next_game_at_ms: 0 };
        assert_eq!(with_games(pause, 5).tiebreak_attempt(), 2);
    }

    #[test]
    fn only_a_decision_or_adjudication_closes_a_match() {
        let closed = [
            Phase::Decided {
                winner: 10,
                decided_at_ms: 0,
            },
            Phase::NeedsAdjudication {
                adjudication: Adjudication::ContestEnded,
            },
        ];
        assert!(closed.iter().all(Phase::is_closed));
        let open = [Phase::Ordering, Phase::Preparing, Phase::Playing, blocked()];
        assert!(!open.iter().any(Phase::is_closed));
    }
}
