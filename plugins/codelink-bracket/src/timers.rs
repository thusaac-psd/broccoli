//! Timer keys and timer delivery.
//!
//! [`TimerKey`] is the one place timer keys are written and read. Every match
//! wakeup ticks its match ([`crate::executor::drive`]); its purpose only keeps
//! different phases' wakeups apart, so scheduling one never replaces another.
//! A stage wakeup opens its stage.

use broccoli_server_sdk::Host;
use broccoli_server_sdk::error::SdkError;
use serde::{Deserialize, Serialize};

use crate::executor;
use crate::machine::{Closure, MatchTimer};

/// How many times a failing delivery re-arms itself, a second apart, before
/// it is left to the host's own bounded redelivery.
pub const MAX_RETRIES: u32 = 30;
const RETRY_DELAY_MS: i64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerKey {
    Match {
        contest: i32,
        match_id: u8,
        timer: MatchTimer,
    },
    /// Opening a stage. The wake time is part of the key, so a stale
    /// callback for an edited schedule cannot replace a newer, earlier one.
    StageOpen { contest: i32, round: u8, at_ms: i64 },
}

impl TimerKey {
    pub fn encode(&self) -> String {
        match *self {
            TimerKey::Match {
                contest,
                match_id,
                timer,
            } => {
                let purpose = match timer {
                    MatchTimer::Start => "start".to_string(),
                    MatchTimer::OrderingClose => "ordering-close".to_string(),
                    MatchTimer::GameStart => "game-start".to_string(),
                    MatchTimer::Deadline { game } => format!("deadline:{game}"),
                    MatchTimer::Effective {
                        game,
                        submission_id,
                        at_us: None,
                    } => format!("effective:{game}:{submission_id}"),
                    MatchTimer::Effective {
                        game,
                        submission_id,
                        at_us: Some(at),
                    } => format!("effective:{game}:{submission_id}:at:{at}"),
                    MatchTimer::JudgeWait { game } => format!("judge-wait:{game}"),
                    MatchTimer::Intermission { before_game } => {
                        format!("intermission:{before_game}")
                    }
                    MatchTimer::Closing(Closure::Decision) => "closing:decision".to_string(),
                    MatchTimer::Closing(Closure::Adjudication) => {
                        "closing:adjudication".to_string()
                    }
                };
                format!("match:{contest}:{match_id}:{purpose}")
            }
            TimerKey::StageOpen {
                contest,
                round,
                at_ms,
            } => format!("stage:{contest}:{round}:{at_ms}"),
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        let parts: Vec<&str> = key.split(':').collect();
        match parts.as_slice() {
            ["stage", contest, round, at] => Some(TimerKey::StageOpen {
                contest: contest.parse().ok()?,
                round: round.parse().ok()?,
                at_ms: at.parse().ok()?,
            }),
            ["match", contest, match_id, purpose @ ..] => {
                let timer = match purpose {
                    ["start"] => MatchTimer::Start,
                    ["ordering-close"] => MatchTimer::OrderingClose,
                    ["game-start"] => MatchTimer::GameStart,
                    ["deadline", game] => MatchTimer::Deadline {
                        game: game.parse().ok()?,
                    },
                    ["judge-wait", game] => MatchTimer::JudgeWait {
                        game: game.parse().ok()?,
                    },
                    ["intermission", game] => MatchTimer::Intermission {
                        before_game: game.parse().ok()?,
                    },
                    ["closing", "decision"] => MatchTimer::Closing(Closure::Decision),
                    ["closing", "adjudication"] => MatchTimer::Closing(Closure::Adjudication),
                    ["effective", game, submission] => MatchTimer::Effective {
                        game: game.parse().ok()?,
                        submission_id: submission.parse().ok()?,
                        at_us: None,
                    },
                    ["effective", game, submission, "at", at] => MatchTimer::Effective {
                        game: game.parse().ok()?,
                        submission_id: submission.parse().ok()?,
                        at_us: Some(at.parse().ok()?),
                    },
                    _ => return None,
                };
                Some(TimerKey::Match {
                    contest: contest.parse().ok()?,
                    match_id: match_id.parse().ok()?,
                    timer,
                })
            }
            _ => None,
        }
    }
}

/// The payload a re-armed delivery carries: how often it has failed.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct RetryPayload {
    retry: u32,
}

/// Dispatch a timer against current persisted state.
///
/// A failed delivery re-arms the same key a second later, up to
/// [`MAX_RETRIES`] times, so a transient failure in post-commit work is
/// retried promptly; scheduling clears the host's claim, so its cleanup
/// cannot delete the replacement. After that, the error is left to the
/// host's own bounded redelivery, so a permanently failing key stops.
pub fn handle_timer(
    host: &Host,
    key: &str,
    payload: &str,
    fire_at_ms: i64,
) -> Result<(), SdkError> {
    let Some(parsed) = TimerKey::parse(key) else {
        return host
            .log
            .info(&format!("codelink-bracket: unparseable timer key: {key}"));
    };
    let result = match parsed {
        TimerKey::Match {
            contest, match_id, ..
        } => executor::tick(host, contest, match_id),
        TimerKey::StageOpen { contest, round, .. } => {
            crate::stages::try_open_stage(host, contest, round)
        }
    };
    let Err(error) = result else {
        return Ok(());
    };
    let retry = serde_json::from_str::<RetryPayload>(payload)
        .map(|p| p.retry)
        .unwrap_or(0);
    if retry < MAX_RETRIES {
        let next = serde_json::to_string(&RetryPayload { retry: retry + 1 })?;
        host.timer
            .schedule(fire_at_ms.saturating_add(RETRY_DELAY_MS), key, &next)?;
    }
    Err(error)
}

/// `[[server.timers]] function = "on_timer"` entry point. Errors must reach the
/// dispatcher rather than acknowledging a partially completed transition.
#[cfg(target_arch = "wasm32")]
#[extism_pdk::plugin_fn]
pub fn on_timer(input: String) -> extism_pdk::FnResult<String> {
    #[derive(Deserialize)]
    struct TimerCallbackInput {
        key: String,
        #[serde(default)]
        payload: String,
        fire_at_ms: i64,
    }
    let host = Host::new();
    let input: TimerCallbackInput = serde_json::from_str(&input)?;
    handle_timer(&host, &input.key, &input.payload, input.fire_at_ms)?;
    Ok("ok".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_timer_key_round_trips_through_its_encoding() {
        let keys = [
            MatchTimer::Start,
            MatchTimer::OrderingClose,
            MatchTimer::GameStart,
            MatchTimer::Deadline { game: 2 },
            MatchTimer::Effective {
                game: 2,
                submission_id: 41,
                at_us: None,
            },
            MatchTimer::Effective {
                game: 2,
                submission_id: 41,
                at_us: Some(1_500_000),
            },
            MatchTimer::JudgeWait { game: 2 },
            MatchTimer::Intermission { before_game: 3 },
            MatchTimer::Closing(Closure::Decision),
            MatchTimer::Closing(Closure::Adjudication),
        ]
        .map(|timer| TimerKey::Match {
            contest: 7,
            match_id: 3,
            timer,
        })
        .into_iter()
        .chain([TimerKey::StageOpen {
            contest: 7,
            round: 2,
            at_ms: 9_000,
        }]);
        let mut encoded = std::collections::HashSet::new();
        for key in keys {
            assert_eq!(TimerKey::parse(&key.encode()), Some(key));
            assert!(encoded.insert(key.encode()), "keys must be distinct");
        }
    }

    #[test]
    fn foreign_and_malformed_keys_do_not_parse() {
        for key in [
            "",
            "match:7:3",
            "match:7:3:unknown",
            "match:x:3:deadline:0",
            "match:7:3:deadline",
            "match:7:3:deadline:0:extra",
            "match:7:3:closing:other",
            "stage:7:2",
            "other:7:3:deadline",
        ] {
            assert_eq!(TimerKey::parse(key), None, "{key}");
        }
    }

    /// A match wakeup whose delivery fails: no server time can be read.
    fn failing_key(host: &Host) -> String {
        let setup = crate::test_support::setup_two_rounds();
        crate::test_support::seed(host, 7, &setup, 3, &Default::default());
        TimerKey::Match {
            contest: 7,
            match_id: 3,
            timer: MatchTimer::Deadline { game: 0 },
        }
        .encode()
    }

    #[test]
    fn a_failing_delivery_rearms_with_its_retry_count() {
        let host = Host::mock();
        let key = failing_key(&host);
        assert!(handle_timer(&host, &key, "", 5_000).is_err());
        assert_eq!(host.timer.scheduled_at(&key), Some(6_000));
        assert!(handle_timer(&host, &key, r#"{"retry":4}"#, 6_000).is_err());
        assert_eq!(host.timer.scheduled_at(&key), Some(7_000));
    }

    #[test]
    fn a_delivery_that_keeps_failing_stops_rearming() {
        let host = Host::mock();
        let key = failing_key(&host);
        let payload = format!(r#"{{"retry":{MAX_RETRIES}}}"#);
        assert!(handle_timer(&host, &key, &payload, 5_000).is_err());
        assert!(!host.timer.is_scheduled(&key));
    }
}
