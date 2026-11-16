use serde::{Deserialize, Serialize};

/// Where a contest is at one instant. The host derives it from the contest's
/// timeline with one function, on the platform clock, and hands it to plugins
/// wherever they act on a contest; a plugin never derives it itself.
///
/// The activation window is `[activate_time, deactivate_time)` and the
/// contest runs during `[start_time, end_time)`. Timelines satisfy
/// `activate_time <= start_time < end_time <= deactivate_time`, so the phases
/// follow each other in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContestPhase {
    /// `activate_time` is unset or in the future. Only `contest:manage` sees
    /// the contest.
    Unactivated,
    /// Activated, before `start_time`.
    Upcoming,
    /// From `start_time` until just before `end_time`.
    Running,
    /// From `end_time` until the contest is deactivated.
    Ended,
    /// `deactivate_time` has passed. Only `contest:manage` sees the contest.
    Deactivated,
}

impl ContestPhase {
    /// Whether the contest is inside its activation window, i.e. visible to
    /// users without `contest:manage`.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Upcoming | Self::Running | Self::Ended)
    }

    /// Whether the contest is active and has started (running or ended).
    pub fn has_started(self) -> bool {
        matches!(self, Self::Running | Self::Ended)
    }

    /// Whether the contest's end time has passed: ended, or deactivated
    /// after it ended.
    pub fn has_ended(self) -> bool {
        matches!(self, Self::Ended | Self::Deactivated)
    }

    /// The three stages a scoreboard shows: `"before"` the start (or before
    /// activation), `"during"` the contest, `"after"` its end.
    pub fn stage(self) -> &'static str {
        match self {
            Self::Unactivated | Self::Upcoming => "before",
            Self::Running => "during",
            Self::Ended | Self::Deactivated => "after",
        }
    }
}

/// The contest a plugin route addresses, resolved and checked by the host
/// before the plugin runs (see the route's `contest` key in `plugin.toml`).
/// The caller may read this contest, and it has the route's `contest_type`
/// when the route declares one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteContest {
    pub id: i32,
    pub contest_type: Option<String>,
    pub is_public: bool,
    pub phase: ContestPhase,
    /// Whether the caller is registered in the contest.
    pub participant: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_uses_snake_case_on_the_wire() {
        assert_eq!(
            serde_json::to_string(&ContestPhase::Unactivated).unwrap(),
            r#""unactivated""#
        );
        let back: ContestPhase = serde_json::from_str(r#""running""#).unwrap();
        assert_eq!(back, ContestPhase::Running);
    }

    #[test]
    fn phase_predicates_follow_the_timeline() {
        use ContestPhase::*;
        let cases = [
            (Unactivated, false, false, false, "before"),
            (Upcoming, true, false, false, "before"),
            (Running, true, true, false, "during"),
            (Ended, true, true, true, "after"),
            (Deactivated, false, false, true, "after"),
        ];
        for (phase, active, started, ended, stage) in cases {
            assert_eq!(phase.is_active(), active, "{phase:?}");
            assert_eq!(phase.has_started(), started, "{phase:?}");
            assert_eq!(phase.has_ended(), ended, "{phase:?}");
            assert_eq!(phase.stage(), stage, "{phase:?}");
        }
    }
}
