//! Rolling worker restart / cache-leader race scenario.
//!
//! This probes the Redis contract used by the worker cache leader election:
//! a follower must observe the leader-held lease and poll rather than compile,
//! then a follower must be able to become leader once the abandoned lease
//! expires after the worker restart.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::process::Command;
use tokio::time::sleep;
use uuid::Uuid;

use super::{Scenario, ScenarioContext, ScenarioOutcome};
use crate::fault::transcript::{EventKind, Severity, Transcript};

const CACHE_LEADER_PREFIX: &str = "broccoli:cache-leader:";

pub struct RollingWorkerRestart {
    pub leader_ttl_secs: u64,
    pub leader_hold_ms: u64,
    pub follower_poll_interval_ms: u64,
}

impl Default for RollingWorkerRestart {
    fn default() -> Self {
        Self {
            leader_ttl_secs: 3,
            leader_hold_ms: 500,
            follower_poll_interval_ms: 100,
        }
    }
}

#[async_trait]
impl Scenario for RollingWorkerRestart {
    fn name(&self) -> &'static str {
        "rolling_worker_restart"
    }

    async fn run(&self, ctx: &ScenarioContext) -> anyhow::Result<ScenarioOutcome> {
        let ttl_secs = self.leader_ttl_secs.max(1);
        let hold_ms = self.leader_hold_ms.max(1);
        let poll_ms = self.follower_poll_interval_ms.max(1);
        let mut transcript = Transcript::new(self.name())
            .with_param("leader_ttl_secs", ttl_secs.into())
            .with_param("leader_hold_ms", hold_ms.into())
            .with_param("follower_poll_interval_ms", poll_ms.into());

        let client = redis::Client::open(ctx.redis_url.as_str())?;
        let mut conn = match client.get_multiplexed_async_connection().await {
            Ok(conn) => conn,
            Err(e) => {
                transcript.record(
                    EventKind::Setup,
                    Severity::Error,
                    format!("redis connect failed: {e}"),
                );
                transcript.finish(false);
                return Ok(ScenarioOutcome {
                    passed: false,
                    transcript,
                });
            }
        };
        let pong: redis::RedisResult<String> = redis::cmd("PING").query_async(&mut conn).await;
        if let Err(e) = pong {
            transcript.record(
                EventKind::Setup,
                Severity::Error,
                format!("redis PING failed: {e}"),
            );
            transcript.finish(false);
            return Ok(ScenarioOutcome {
                passed: false,
                transcript,
            });
        }

        let run_id = Uuid::now_v7().to_string();
        let lock_key = cache_leader_key(&format!("stress-test:{run_id}"));
        let leader_token = format!("leader-{run_id}");
        let follower_token = format!("follower-{run_id}");

        transcript.record_with(
            EventKind::Setup,
            Severity::Info,
            "prepared cache-leader lock key",
            [("lock_key".to_string(), serde_json::json!(lock_key))]
                .into_iter()
                .collect(),
        );

        match try_acquire(&mut conn, &lock_key, &leader_token, ttl_secs).await {
            Ok(true) => {}
            Ok(false) => {
                transcript.record(
                    EventKind::Inject,
                    Severity::Error,
                    "initial leader could not acquire cache-leader lock",
                );
                transcript.finish(false);
                return Ok(ScenarioOutcome {
                    passed: false,
                    transcript,
                });
            }
            Err(e) => return Err(e),
        }

        transcript.record(
            EventKind::Inject,
            Severity::Info,
            "compile leader acquired cache-leader lease",
        );

        let hold_deadline = Instant::now() + Duration::from_millis(hold_ms);
        let mut follower_poll_count = 0u64;
        let mut split_brain = false;
        while Instant::now() < hold_deadline {
            follower_poll_count += 1;
            if try_acquire(&mut conn, &lock_key, &follower_token, ttl_secs).await? {
                split_brain = true;
                break;
            }
            sleep(Duration::from_millis(poll_ms)).await;
        }

        transcript.record_with(
            EventKind::Observe,
            if split_brain {
                Severity::Error
            } else {
                Severity::Info
            },
            if split_brain {
                "follower acquired lock while leader lease was still active"
            } else {
                "follower observed active leader lease and stayed on poll path"
            },
            [(
                "follower_poll_count".to_string(),
                serde_json::json!(follower_poll_count),
            )]
            .into_iter()
            .collect(),
        );

        if split_brain {
            cleanup_key(&mut conn, &lock_key).await?;
            transcript.finish(false);
            return Ok(ScenarioOutcome {
                passed: false,
                transcript,
            });
        }

        if let Some(command) = ctx.kill_command.as_deref() {
            let status = Command::new("sh").arg("-c").arg(command).status().await?;
            transcript.record_with(
                EventKind::Inject,
                if status.success() {
                    Severity::Info
                } else {
                    Severity::Error
                },
                "restart command completed",
                [
                    ("command".to_string(), serde_json::json!(command)),
                    ("status".to_string(), serde_json::json!(status.code())),
                ]
                .into_iter()
                .collect(),
            );
            if !status.success() {
                cleanup_key(&mut conn, &lock_key).await?;
                transcript.finish(false);
                return Ok(ScenarioOutcome {
                    passed: false,
                    transcript,
                });
            }
        } else {
            transcript.record(
                EventKind::Inject,
                Severity::Info,
                "no restart command supplied; simulating abrupt leader death by abandoning lease",
            );
        }

        let takeover_started = Instant::now();
        let takeover_deadline = takeover_started + Duration::from_secs(ttl_secs + 2);
        let mut takeover_polls = 0u64;
        let mut takeover_elapsed_ms = None;
        while Instant::now() < takeover_deadline {
            takeover_polls += 1;
            if try_acquire(&mut conn, &lock_key, &follower_token, ttl_secs).await? {
                takeover_elapsed_ms = Some(takeover_started.elapsed().as_millis() as u64);
                break;
            }
            sleep(Duration::from_millis(poll_ms)).await;
        }

        cleanup_key(&mut conn, &lock_key).await?;

        let takeover_ok = takeover_elapsed_ms.is_some();
        transcript.record_with(
            EventKind::Recover,
            if takeover_ok {
                Severity::Info
            } else {
                Severity::Error
            },
            if takeover_ok {
                "follower acquired cache-leader lease after abandoned leader expired"
            } else {
                "follower never acquired cache-leader lease after abandoned leader expired"
            },
            [
                (
                    "takeover_polls".to_string(),
                    serde_json::json!(takeover_polls),
                ),
                (
                    "takeover_elapsed_ms".to_string(),
                    serde_json::json!(takeover_elapsed_ms),
                ),
            ]
            .into_iter()
            .collect(),
        );

        let passed = follower_poll_count > 0 && takeover_ok;
        transcript.record(
            EventKind::Assertion,
            if passed {
                Severity::Info
            } else {
                Severity::Error
            },
            if passed {
                "rolling restart race passed: follower path observed and takeover succeeded"
            } else {
                "rolling restart race failed"
            },
        );
        transcript.set_summary(
            "follower_poll_count",
            serde_json::json!(follower_poll_count),
        );
        transcript.set_summary(
            "takeover_elapsed_ms",
            serde_json::json!(takeover_elapsed_ms),
        );
        transcript.record(EventKind::Summary, Severity::Info, "scenario complete");
        transcript.finish(passed);

        Ok(ScenarioOutcome { passed, transcript })
    }
}

fn cache_leader_key(cache_key: &str) -> String {
    format!("{CACHE_LEADER_PREFIX}{cache_key}")
}

async fn try_acquire(
    conn: &mut redis::aio::MultiplexedConnection,
    key: &str,
    token: &str,
    ttl_secs: u64,
) -> anyhow::Result<bool> {
    let result: Option<String> = redis::cmd("SET")
        .arg(key)
        .arg(token)
        .arg("NX")
        .arg("EX")
        .arg(ttl_secs)
        .query_async(conn)
        .await?;
    Ok(result.is_some())
}

async fn cleanup_key(
    conn: &mut redis::aio::MultiplexedConnection,
    key: &str,
) -> anyhow::Result<()> {
    let _: () = redis::cmd("DEL").arg(key).query_async(conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_leader_key_uses_worker_prefix() {
        assert_eq!(
            cache_leader_key("compile-key"),
            "broccoli:cache-leader:compile-key"
        );
    }

    #[test]
    fn defaults_are_short_enough_for_fault_harness() {
        let scenario = RollingWorkerRestart::default();
        assert!(scenario.leader_ttl_secs <= 5);
        assert!(scenario.follower_poll_interval_ms <= 250);
    }
}
