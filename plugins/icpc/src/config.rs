use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContestConfig {
    /// Minutes of penalty per wrong submission before AC (default: 20).
    #[serde(default = "default_penalty_minutes")]
    pub penalty_minutes: i32,

    /// Whether compilation errors count as penalty attempts (default: false).
    #[serde(default)]
    pub count_compile_error: bool,

    /// Whether contestants see per-test-case verdicts (default: false).
    #[serde(default)]
    pub show_test_details: bool,

    /// Whether contestants see the FULL live scoreboard during the contest
    /// (standard public-ICPC behavior). Default false = each contestant sees only
    /// their own row until the contest ends; organizers (contest:manage) always
    /// see all rows.
    #[serde(default)]
    pub public_standings: bool,

    /// Scoreboard freeze window in minutes before the contest end (0 = disabled).
    /// Like real ICPC: in the final N minutes, a contestant's view stops updating
    /// rank/solved/penalty, and submissions made during the freeze show as PENDING
    /// ("?") instead of AC/WA. Organizers (contest:manage) always see the real
    /// board; the board unfreezes for everyone once the contest ends.
    #[serde(default)]
    pub freeze_minutes: i32,
}

fn default_penalty_minutes() -> i32 {
    20
}

/// The freeze-gating decision for one viewer, computed purely (no DB/host) so it
/// can be unit-tested. `freeze_start_ms` is the elapsed time at/after which a
/// submission is frozen (`i64::MAX` = nothing is frozen).
#[derive(Debug, Clone, PartialEq)]
pub struct FreezeView {
    pub freeze_start_ms: i64,
    pub frozen: bool,
    pub can_reveal: bool,
}

/// Elapsed-ms offset (from contest start) at which the freeze window opens.
/// `i64::MAX` means there is no freeze. Shared by `freeze_view` and the reveal
/// endpoint so both agree on where the window starts.
pub fn freeze_window_start_ms(freeze_minutes: i32, duration_ms: i64) -> i64 {
    if freeze_minutes > 0 {
        (duration_ms - i64::from(freeze_minutes) * 60_000).max(0)
    } else {
        i64::MAX
    }
}

/// A freeze window in epoch milliseconds: from its opening to the contest end.
/// The reveal record stores the window an organizer revealed, and the board
/// counts as revealed only while the contest's current window is that same
/// one. Moving the start or end time, or changing `freeze_minutes`, after a
/// reveal therefore freezes the board again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreezeWindow {
    pub from_ms: i64,
    pub until_ms: i64,
}

impl FreezeWindow {
    /// The contest's current window, `None` when no freeze is configured.
    pub fn current(freeze_minutes: i32, start_epoch_ms: i64, duration_ms: i64) -> Option<Self> {
        (freeze_minutes > 0).then(|| Self {
            from_ms: start_epoch_ms + freeze_window_start_ms(freeze_minutes, duration_ms),
            until_ms: start_epoch_ms + duration_ms,
        })
    }
}

/// Storage key of a contest's reveal record.
pub fn reveal_key(contest_id: i32) -> String {
    format!("reveal:{contest_id}")
}

/// Whether the stored reveal record reveals the contest's current window. A
/// missing or unreadable record reveals nothing.
pub fn is_revealed(record: Option<&str>, current: Option<FreezeWindow>) -> bool {
    let revealed = record.and_then(|r| serde_json::from_str::<FreezeWindow>(r).ok());
    revealed.is_some() && revealed == current
}

/// Whether the freeze window is currently OPEN: freeze configured, in the
/// 'during'/'after' phase, past the window start, and not yet revealed.
pub fn freeze_window_open(
    freeze_minutes: i32,
    phase: &str,
    revealed: bool,
    duration_ms: i64,
    now_elapsed_ms: i64,
) -> bool {
    let in_window_phase = phase == "during" || phase == "after";
    freeze_minutes > 0
        && !revealed
        && in_window_phase
        && now_elapsed_ms >= freeze_window_start_ms(freeze_minutes, duration_ms)
}

/// Decide the freeze view. Freeze holds for a CONTESTANT (`!can_view_all`) once
/// the window opens and until an organizer reveals, spanning the 'during' and
/// 'after' phases. Organizers are never frozen and may reveal once the contest
/// has ended with the board still frozen. No freeze before the contest, when
/// `freeze_minutes == 0`, or once revealed.
pub fn freeze_view(
    freeze_minutes: i32,
    phase: &str,
    can_view_all: bool,
    revealed: bool,
    duration_ms: i64,
    now_elapsed_ms: i64,
) -> FreezeView {
    let window_start = freeze_window_start_ms(freeze_minutes, duration_ms);
    let window_open =
        freeze_window_open(freeze_minutes, phase, revealed, duration_ms, now_elapsed_ms);
    FreezeView {
        // Only a frozen CONTESTANT view splits submissions at the window start.
        freeze_start_ms: if window_open && !can_view_all {
            window_start
        } else {
            i64::MAX
        },
        frozen: window_open && !can_view_all,
        can_reveal: window_open && can_view_all && phase == "after",
    }
}

/// Per-user per-problem state derived from current finalized judgements.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProblemState {
    /// Number of penalty-eligible wrong submissions before AC.
    pub attempts: i32,
    /// Whether the problem has been solved.
    pub solved: bool,
    /// Milliseconds from contest start to the accepted submission.
    /// Only meaningful when `solved == true`.
    pub solve_time_ms: Option<i64>,
}

impl ProblemState {
    /// Penalty time in minutes for this problem. 0 if unsolved.
    pub fn penalty_minutes(&self, penalty_per_attempt: i32) -> i32 {
        if !self.solved {
            return 0;
        }
        let solve_min = self.solve_time_ms.unwrap_or(0).div_euclid(60_000) as i32;
        solve_min + self.attempts * penalty_per_attempt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_penalty_minutes() {
        let config: ContestConfig = serde_json::from_str(r#"{"penalty_minutes": 10}"#).unwrap();
        assert_eq!(config.penalty_minutes, 10);
    }

    // duration 300min, freeze 60min -> freeze window opens at 240min elapsed.
    const DUR: i64 = 300 * 60_000;
    const FREEZE_START: i64 = 240 * 60_000;

    #[test]
    fn freeze_off_before_contest_or_when_zero() {
        let before = freeze_view(60, "before", false, false, DUR, 0);
        assert!(!before.frozen && !before.can_reveal && before.freeze_start_ms == i64::MAX);
        let zero = freeze_view(0, "during", false, false, DUR, 290 * 60_000);
        assert!(!zero.frozen && !zero.can_reveal && zero.freeze_start_ms == i64::MAX);
    }

    #[test]
    fn contestant_frozen_only_inside_window() {
        // before the window opens: not frozen, and no split (freeze_start = MAX).
        let before = freeze_view(60, "during", false, false, DUR, 200 * 60_000);
        assert!(!before.frozen, "before the window: live");
        assert_eq!(
            before.freeze_start_ms,
            i64::MAX,
            "no split before the window"
        );
        // inside the window: frozen, split at the window start.
        let inside = freeze_view(60, "during", false, false, DUR, 250 * 60_000);
        assert!(inside.frozen, "inside the window: frozen");
        assert_eq!(inside.freeze_start_ms, FREEZE_START);
    }

    #[test]
    fn contestant_frozen_through_after_until_revealed() {
        let now = 400 * 60_000; // past end
        let unrevealed = freeze_view(60, "after", false, false, DUR, now);
        assert!(unrevealed.frozen);
        assert_eq!(unrevealed.freeze_start_ms, FREEZE_START);
        let revealed = freeze_view(60, "after", false, true, DUR, now);
        assert!(!revealed.frozen && revealed.freeze_start_ms == i64::MAX);
    }

    #[test]
    fn organizer_reveals_only_after_the_contest_ends() {
        let before_window = freeze_view(60, "during", true, false, DUR, 100 * 60_000);
        assert!(!before_window.frozen, "organizer never frozen");
        assert!(
            !before_window.can_reveal,
            "no reveal before the window opens"
        );
        assert_eq!(
            before_window.freeze_start_ms,
            i64::MAX,
            "organizer never split"
        );
        // Mid-contest, even inside the window, the board stays frozen.
        assert!(!freeze_view(60, "during", true, false, DUR, 250 * 60_000).can_reveal);
        assert!(freeze_view(60, "after", true, false, DUR, 400 * 60_000).can_reveal);
        assert!(!freeze_view(60, "after", true, true, DUR, 400 * 60_000).can_reveal);
    }

    #[test]
    fn freeze_window_open_helper_matrix() {
        assert!(!freeze_window_open(60, "before", false, DUR, 0));
        assert!(!freeze_window_open(60, "during", false, DUR, 200 * 60_000)); // pre-window
        assert!(freeze_window_open(60, "during", false, DUR, 250 * 60_000)); // in window
        assert!(freeze_window_open(60, "after", false, DUR, 400 * 60_000)); // after
        assert!(!freeze_window_open(60, "after", true, DUR, 400 * 60_000)); // revealed
        assert!(!freeze_window_open(0, "during", false, DUR, 290 * 60_000)); // disabled
    }

    #[test]
    fn a_reveal_lasts_only_while_the_revealed_window_is_current() {
        const START: i64 = 1_700_000_000_000;
        let revealed = FreezeWindow::current(60, START, DUR);
        let record = serde_json::to_string(&revealed.unwrap()).unwrap();
        assert!(is_revealed(Some(&record), revealed));

        let longer_freeze = FreezeWindow::current(90, START, DUR);
        let extended_end = FreezeWindow::current(60, START, DUR + 30 * 60_000);
        let moved_start = FreezeWindow::current(60, START + 60_000, DUR);
        for changed in [longer_freeze, extended_end, moved_start] {
            assert!(!is_revealed(Some(&record), changed), "{changed:?}");
        }
        assert!(!is_revealed(
            Some(&record),
            FreezeWindow::current(0, START, DUR)
        ));
        assert!(!is_revealed(None, revealed));
        assert!(!is_revealed(Some("garbage"), revealed));
    }

    #[test]
    fn penalty_minutes_unsolved() {
        let state = ProblemState {
            attempts: 5,
            solved: false,
            solve_time_ms: None,
        };
        assert_eq!(state.penalty_minutes(20), 0);
    }

    #[test]
    fn penalty_minutes_solved() {
        let state = ProblemState {
            attempts: 2,
            solved: true,
            solve_time_ms: Some(45 * 60_000), // 45 minutes
        };
        // 45 + 2*20 = 85
        assert_eq!(state.penalty_minutes(20), 85);
    }

    #[test]
    fn penalty_minutes_solved_no_wrong_attempts() {
        let state = ProblemState {
            attempts: 0,
            solved: true,
            solve_time_ms: Some(30 * 60_000),
        };
        assert_eq!(state.penalty_minutes(20), 30);
    }

    #[test]
    fn penalty_minutes_sub_minute() {
        let state = ProblemState {
            attempts: 0,
            solved: true,
            solve_time_ms: Some(30_000), // 30 seconds
        };
        // 0 minutes (truncated) + 0 penalties = 0
        assert_eq!(state.penalty_minutes(20), 0);
    }
}
