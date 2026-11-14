//! Server-replica kill scenario.
//!
//! Validates the lease/steal + stuck-redispatch recovery path. The operator
//! must bring their own running Broccoli stack (server + worker + Postgres +
//! Redis) and pre-seed a problem whose ID is passed via `--problem-id`.
//!
//! The harness:
//!   1. POSTs N submissions to `${server_url}/api/v1/problems/{id}/submissions`
//!      as the admin, through [`crate::client::Client`].
//!   2. Polls Postgres until at least one submission has reached
//!      `Compiling`/`Running` with a non-NULL `owner_server_id`.
//!   3. Executes `${kill_command}` via `sh -c` (e.g.
//!      `docker compose kill server-1`).
//!   4. Polls for up to `observe_timeout_secs` (default 75s) watching for recovery: either ownership transferred to
//!      a different server, status reset to Pending, or terminal Judged.
//!   5. Passes if every previously-in-flight submission reached a recovery
//!      state within the window; fails otherwise.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;
use tokio::process::Command;

use super::{Scenario, ScenarioContext, ScenarioOutcome};
use crate::client::{AuthCreds, Client};
use crate::dto::{CreateSubmissionRequest, SubmissionFileDto};
use crate::fault::transcript::{EventKind, Severity, Transcript};

pub struct KillServerRecovery {
    pub server_url: String,
    pub creds: AuthCreds,
    pub submission_count: usize,
    pub observe_timeout_secs: u64,
    pub problem_id: i32,
}

/// A snapshot of the columns we care about for a submission: its status and
/// the dispatch state of its current judgement. Kept narrow so the harness can
/// hand-roll the SELECT without depending on the server crate's SeaORM
/// entities.
#[derive(Debug, Clone, PartialEq)]
pub struct SubmissionRow {
    pub id: i32,
    pub status: String,
    pub owner_server_id: Option<String>,
    pub lease_heartbeat_at: Option<DateTime<Utc>>,
    pub dispatch_retries: i32,
}

/// The outcome of comparing a submission's row before vs. after the kill.
/// Pure decision function - testable without a live stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryStatus {
    /// Submission reached a terminal Judged state - full recovery.
    Judged,
    /// Owner moved to a different server - lease/steal recovered the row.
    OwnerChanged,
    /// Status reset to Pending - stuck-detector re-dispatched.
    StatusReset,
    /// No recovery signal yet; submission still owned by the dead replica.
    Stuck,
}

/// Pure helper: decide whether `after` shows recovery from `before`.
///
/// We treat lease_heartbeat refresh on the *same* dead owner as still-stuck,
/// because in production the killed replica cannot ping. Only an owner
/// *change* counts as lease/steal recovery.
pub fn classify_recovery(before: &SubmissionRow, after: &SubmissionRow) -> RecoveryStatus {
    if after.status == "Judged" {
        return RecoveryStatus::Judged;
    }
    match (
        before.owner_server_id.as_deref(),
        after.owner_server_id.as_deref(),
    ) {
        (Some(b), Some(a)) if a != b => return RecoveryStatus::OwnerChanged,
        (Some(_), None) => return RecoveryStatus::OwnerChanged,
        _ => {}
    }
    if before.status != "Pending" && after.status == "Pending" {
        return RecoveryStatus::StatusReset;
    }
    RecoveryStatus::Stuck
}

#[async_trait]
impl Scenario for KillServerRecovery {
    fn name(&self) -> &'static str {
        "kill_server_recovery"
    }

    async fn run(&self, ctx: &ScenarioContext) -> anyhow::Result<ScenarioOutcome> {
        let mut transcript = Transcript::new(self.name())
            .with_param("submission_count", self.submission_count.into())
            .with_param("observe_timeout_secs", self.observe_timeout_secs.into())
            .with_param("problem_id", self.problem_id.into());

        let db_url = ctx
            .db_url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("kill_server_recovery requires --db-url"))?;
        let kill_command = ctx
            .kill_command
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("kill_server_recovery requires --kill-command"))?;

        // ---- Setup: validate prerequisites. -------------------------------
        let pool = match PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(5))
            .connect(db_url)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                transcript.record(
                    EventKind::Setup,
                    Severity::Error,
                    format!("postgres connect failed: {e}"),
                );
                transcript.finish(false);
                return Ok(ScenarioOutcome {
                    passed: false,
                    transcript,
                });
            }
        };

        if let Err(e) = sqlx::query("SELECT 1").execute(&pool).await {
            transcript.record(
                EventKind::Setup,
                Severity::Error,
                format!("postgres SELECT 1 failed: {e}"),
            );
            transcript.finish(false);
            return Ok(ScenarioOutcome {
                passed: false,
                transcript,
            });
        }

        match redis::Client::open(ctx.redis_url.as_str()) {
            Ok(client) => match client.get_multiplexed_async_connection().await {
                Ok(mut conn) => {
                    let pong: redis::RedisResult<String> =
                        redis::cmd("PING").query_async(&mut conn).await;
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
                }
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
            },
            Err(e) => {
                transcript.record(
                    EventKind::Setup,
                    Severity::Error,
                    format!("redis client open failed: {e}"),
                );
                transcript.finish(false);
                return Ok(ScenarioOutcome {
                    passed: false,
                    transcript,
                });
            }
        }

        let client = match Client::new(self.server_url.clone(), self.creds.clone()).await {
            Ok(client) => client,
            Err(e) => {
                transcript.record(
                    EventKind::Setup,
                    Severity::Error,
                    format!("admin login failed: {e}"),
                );
                transcript.finish(false);
                return Ok(ScenarioOutcome {
                    passed: false,
                    transcript,
                });
            }
        };
        if let Err(e) = client.get_public_bytes("/healthz").await {
            transcript.record(
                EventKind::Setup,
                Severity::Error,
                format!("server /healthz failed: {e}"),
            );
            transcript.finish(false);
            return Ok(ScenarioOutcome {
                passed: false,
                transcript,
            });
        }

        transcript.record(
            EventKind::Setup,
            Severity::Info,
            "prerequisites OK (postgres, redis, server)",
        );

        // ---- Inject phase 1: post submissions. ----------------------------
        let request = CreateSubmissionRequest {
            files: vec![SubmissionFileDto {
                filename: "main.py".into(),
                content: "print('hi')\n".into(),
            }],
            language: "python3".into(),
            contest_type: None,
        };

        let mut submission_ids: Vec<i32> = Vec::with_capacity(self.submission_count);
        for i in 0..self.submission_count {
            match client.create_submission(self.problem_id, &request).await {
                Ok(submission) => submission_ids.push(submission.id),
                Err(e) => {
                    transcript.record(
                        EventKind::Setup,
                        Severity::Error,
                        format!("submission #{i} failed: {e}"),
                    );
                    transcript.finish(false);
                    return Ok(ScenarioOutcome {
                        passed: false,
                        transcript,
                    });
                }
            }
        }
        transcript.record_with(
            EventKind::Setup,
            Severity::Info,
            "submissions posted",
            BTreeMap::from_iter([("submission_ids".into(), serde_json::json!(submission_ids))]),
        );

        // ---- Wait for at least one to reach an in-progress state. ---------
        let poll_deadline = Instant::now() + Duration::from_secs(30);
        let mut in_flight_seen_at: Option<Instant> = None;
        let mut in_flight_count = 0usize;
        while Instant::now() < poll_deadline {
            let rows = fetch_rows(&pool, &submission_ids).await?;
            in_flight_count = rows
                .iter()
                .filter(|r| {
                    matches!(r.status.as_str(), "Compiling" | "Running")
                        && r.owner_server_id.is_some()
                })
                .count();
            if in_flight_count >= 1 {
                in_flight_seen_at = Some(Instant::now());
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        if in_flight_seen_at.is_none() {
            transcript.record(
                EventKind::Setup,
                Severity::Error,
                "no submission reached an in-progress state within 30s; worker likely not running",
            );
            transcript.finish(false);
            return Ok(ScenarioOutcome {
                passed: false,
                transcript,
            });
        }
        transcript.record_with(
            EventKind::Setup,
            Severity::Info,
            "reached in-progress state",
            BTreeMap::from_iter([("in_flight_count".into(), serde_json::json!(in_flight_count))]),
        );

        // Snapshot baseline for every submission (the "before" picture).
        let before_rows = fetch_rows(&pool, &submission_ids).await?;
        let before_map: BTreeMap<i32, SubmissionRow> =
            before_rows.iter().map(|r| (r.id, r.clone())).collect();

        // Track which submissions were actually owned by a server pre-kill.
        // These are the ones we'll require recovery for. Submissions still in
        // Pending with no owner aren't "stuck on the dead replica."
        let watched_ids: Vec<i32> = before_rows
            .iter()
            .filter(|r| r.owner_server_id.is_some() && r.status != "Judged")
            .map(|r| r.id)
            .collect();

        // ---- Inject phase 2: execute kill. --------------------------------
        let kill_at = Instant::now();
        let kill_result = Command::new("sh")
            .arg("-c")
            .arg(kill_command)
            .output()
            .await?;
        let stdout = String::from_utf8_lossy(&kill_result.stdout).to_string();
        let stderr = String::from_utf8_lossy(&kill_result.stderr).to_string();
        let exit_code = kill_result.status.code().unwrap_or(-1);
        if !kill_result.status.success() {
            transcript.record_with(
                EventKind::Inject,
                Severity::Error,
                "kill command failed",
                BTreeMap::from_iter([
                    ("kill_command".into(), serde_json::json!(kill_command)),
                    ("exit_code".into(), serde_json::json!(exit_code)),
                    ("stdout".into(), serde_json::json!(stdout)),
                    ("stderr".into(), serde_json::json!(stderr)),
                ]),
            );
            transcript.finish(false);
            return Ok(ScenarioOutcome {
                passed: false,
                transcript,
            });
        }
        transcript.record_with(
            EventKind::Inject,
            Severity::Warn,
            "killed server replica",
            BTreeMap::from_iter([
                ("kill_command".into(), serde_json::json!(kill_command)),
                ("exit_code".into(), serde_json::json!(exit_code)),
                ("stdout".into(), serde_json::json!(stdout)),
                ("stderr".into(), serde_json::json!(stderr)),
                (
                    "watched_submission_ids".into(),
                    serde_json::json!(watched_ids),
                ),
            ]),
        );

        // ---- Observe phase: poll for recovery. ----------------------------
        let observe_deadline = kill_at + Duration::from_secs(self.observe_timeout_secs);
        let mut recovered: BTreeMap<i32, (RecoveryStatus, u128)> = BTreeMap::new();

        while Instant::now() < observe_deadline {
            if recovered.len() == watched_ids.len() {
                break;
            }
            let now_rows = fetch_rows(&pool, &watched_ids).await?;
            for after in &now_rows {
                if recovered.contains_key(&after.id) {
                    continue;
                }
                let before = match before_map.get(&after.id) {
                    Some(b) => b,
                    None => continue,
                };
                let status = classify_recovery(before, after);
                if status != RecoveryStatus::Stuck {
                    let elapsed_ms = kill_at.elapsed().as_millis();
                    recovered.insert(after.id, (status, elapsed_ms));
                    transcript.record_with(
                        EventKind::Observe,
                        Severity::Info,
                        format!("submission {} recovered: {:?}", after.id, status),
                        BTreeMap::from_iter([
                            ("submission_id".into(), serde_json::json!(after.id)),
                            (
                                "recovery_kind".into(),
                                serde_json::json!(format!("{status:?}")),
                            ),
                            (
                                "time_to_recovery_ms".into(),
                                serde_json::json!(elapsed_ms as u64),
                            ),
                            (
                                "before_owner".into(),
                                serde_json::json!(before.owner_server_id),
                            ),
                            (
                                "after_owner".into(),
                                serde_json::json!(after.owner_server_id),
                            ),
                            ("after_status".into(), serde_json::json!(after.status)),
                        ]),
                    );
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        // ---- Assertion phase. ---------------------------------------------
        let stuck: Vec<i32> = watched_ids
            .iter()
            .filter(|id| !recovered.contains_key(id))
            .copied()
            .collect();
        let passed = stuck.is_empty();
        if passed {
            transcript.record_with(
                EventKind::Assertion,
                Severity::Info,
                format!(
                    "all {} watched submissions recovered within {}s",
                    watched_ids.len(),
                    self.observe_timeout_secs
                ),
                BTreeMap::from_iter([(
                    "recovered_count".into(),
                    serde_json::json!(recovered.len()),
                )]),
            );
        } else {
            transcript.record_with(
                EventKind::Assertion,
                Severity::Error,
                format!(
                    "{} submission(s) remained stuck on the dead owner past {}s",
                    stuck.len(),
                    self.observe_timeout_secs
                ),
                BTreeMap::from_iter([("stuck_submission_ids".into(), serde_json::json!(stuck))]),
            );
        }

        // ---- Recover phase: nothing automatic; tell the operator. ---------
        transcript.record(
            EventKind::Recover,
            Severity::Info,
            "operator should restart killed replica to restore full capacity",
        );

        // Summary.
        let recovery_kinds: BTreeMap<String, u32> =
            recovered
                .values()
                .fold(BTreeMap::new(), |mut acc, (kind, _)| {
                    *acc.entry(format!("{kind:?}")).or_insert(0) += 1;
                    acc
                });
        transcript.set_summary("watched_count", serde_json::json!(watched_ids.len()));
        transcript.set_summary("recovered_count", serde_json::json!(recovered.len()));
        transcript.set_summary("stuck_count", serde_json::json!(stuck.len()));
        transcript.set_summary("recovery_kinds", serde_json::json!(recovery_kinds));
        let max_ttr = recovered.values().map(|(_, ms)| *ms).max().unwrap_or(0);
        transcript.set_summary("max_time_to_recovery_ms", serde_json::json!(max_ttr as u64));

        transcript.record(EventKind::Summary, Severity::Info, "scenario complete");
        transcript.finish(passed);

        Ok(ScenarioOutcome { passed, transcript })
    }
}

async fn fetch_rows(pool: &sqlx::PgPool, ids: &[i32]) -> anyhow::Result<Vec<SubmissionRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(
        r#"
        SELECT s.id, s.status, j.owner_server_id, j.lease_heartbeat_at,
               j.dispatch_retries
        FROM submission s
        JOIN submission_judgement j ON j.submission_id = s.id AND j.is_current
        WHERE s.id = ANY($1)
        "#,
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(SubmissionRow {
            id: row.try_get("id")?,
            status: row.try_get("status")?,
            owner_server_id: row.try_get("owner_server_id")?,
            lease_heartbeat_at: row.try_get("lease_heartbeat_at")?,
            dispatch_retries: row.try_get("dispatch_retries")?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i32, status: &str, owner: Option<&str>) -> SubmissionRow {
        SubmissionRow {
            id,
            status: status.into(),
            owner_server_id: owner.map(Into::into),
            lease_heartbeat_at: None,
            dispatch_retries: 0,
        }
    }

    #[test]
    fn classify_recovery_owner_change_is_recovery() {
        let before = row(1, "Running", Some("server-1"));
        let after = row(1, "Running", Some("server-2"));
        assert_eq!(
            classify_recovery(&before, &after),
            RecoveryStatus::OwnerChanged
        );
    }

    #[test]
    fn classify_recovery_owner_cleared_is_recovery() {
        let before = row(1, "Running", Some("server-1"));
        let after = row(1, "Running", None);
        assert_eq!(
            classify_recovery(&before, &after),
            RecoveryStatus::OwnerChanged
        );
    }

    #[test]
    fn classify_recovery_status_reset_is_recovery() {
        let before = row(1, "Running", Some("server-1"));
        let after = row(1, "Pending", Some("server-1"));
        assert_eq!(
            classify_recovery(&before, &after),
            RecoveryStatus::StatusReset
        );
    }

    #[test]
    fn classify_recovery_judged_terminal() {
        let before = row(1, "Running", Some("server-1"));
        let after = row(1, "Judged", Some("server-1"));
        assert_eq!(classify_recovery(&before, &after), RecoveryStatus::Judged);
    }

    #[test]
    fn classify_recovery_same_owner_no_change_is_stuck() {
        let before = row(1, "Running", Some("server-1"));
        let after = row(1, "Running", Some("server-1"));
        assert_eq!(classify_recovery(&before, &after), RecoveryStatus::Stuck);
    }

    #[test]
    fn classify_recovery_judged_wins_over_owner_change() {
        // Even if the owner also changed, Judged is the strongest signal.
        let before = row(1, "Running", Some("server-1"));
        let after = row(1, "Judged", Some("server-2"));
        assert_eq!(classify_recovery(&before, &after), RecoveryStatus::Judged);
    }
}
