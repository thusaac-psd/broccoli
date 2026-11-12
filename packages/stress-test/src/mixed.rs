use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use hdrhistogram::Histogram;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep};

use crate::bootstrap::BootstrapState;
use crate::client::Client;
use crate::dto::{
    BulkAddParticipantsRequest, CodeRunResponse, CreateSubmissionRequest, CreateUserEntry,
    CustomTestCaseInput, RunCodeRequest, SubmissionFileDto, SubmissionResponse,
};
use crate::error::{StressError, StressResult};
use crate::events::{Event, Phase};
use crate::scenarios::Scenario;

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const HISTOGRAM_LOW: u64 = 1;
const HISTOGRAM_HIGH: u64 = 600_000;
const HISTOGRAM_SIGFIG: u8 = 3;

#[derive(Debug, Clone)]
pub struct MixedConfig {
    pub total: u64,
    pub duration: Option<Duration>,
    pub rate: u32,
    pub concurrency: u32,
    pub per_job_timeout: Duration,
    pub seed: u64,
    pub contestants: u32,
    pub final_burst_duration: Duration,
    pub final_burst_multiplier: u32,
}

#[derive(Debug)]
pub struct MixedOutcome {
    pub total: u64,
    pub completed: u64,
    pub histogram: Histogram<u64>,
    pub errors: Vec<(u64, String)>,
    pub by_action: HashMap<MixedAction, ActionStats>,
    pub passed_overall: bool,
}

impl MixedOutcome {
    fn empty(total: u64) -> Self {
        Self {
            total,
            completed: 0,
            histogram: Histogram::<u64>::new_with_bounds(
                HISTOGRAM_LOW,
                HISTOGRAM_HIGH,
                HISTOGRAM_SIGFIG,
            )
            .expect("static histogram bounds are valid"),
            errors: Vec::new(),
            by_action: HashMap::new(),
            passed_overall: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum MixedAction {
    FrontendAsset,
    SessionLogin,
    ContestRead,
    ProblemRead,
    SampleRead,
    AttachmentRead,
    CodeRun,
    OfficialSubmission,
    ScoreboardRead,
}

impl MixedAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::SessionLogin => "session_login",
            Self::FrontendAsset => "frontend_asset",
            Self::ContestRead => "contest_read",
            Self::ProblemRead => "problem_read",
            Self::SampleRead => "sample_read",
            Self::AttachmentRead => "attachment_read",
            Self::CodeRun => "code_run",
            Self::OfficialSubmission => "official_submission",
            Self::ScoreboardRead => "scoreboard_read",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ActionStats {
    pub ok: u64,
    pub error: u64,
}

pub fn build_action_mix(include_login: bool) -> Vec<MixedAction> {
    let mut actions = Vec::with_capacity(100);
    if include_login {
        actions.extend(std::iter::repeat_n(MixedAction::SessionLogin, 5));
        actions.extend(std::iter::repeat_n(MixedAction::ContestRead, 15));
        actions.extend(std::iter::repeat_n(MixedAction::ProblemRead, 20));
    } else {
        actions.extend(std::iter::repeat_n(MixedAction::ContestRead, 20));
        actions.extend(std::iter::repeat_n(MixedAction::ProblemRead, 20));
    }
    actions.extend(std::iter::repeat_n(MixedAction::ScoreboardRead, 20));
    actions.extend(std::iter::repeat_n(MixedAction::OfficialSubmission, 15));
    actions.extend(std::iter::repeat_n(MixedAction::CodeRun, 10));
    actions.extend(std::iter::repeat_n(MixedAction::SampleRead, 5));
    actions.extend(std::iter::repeat_n(MixedAction::AttachmentRead, 5));
    actions.extend(std::iter::repeat_n(MixedAction::FrontendAsset, 5));
    actions
}

pub fn scoreboard_path(contest_type: &str, contest_id: i32) -> Option<String> {
    match contest_type {
        "icpc" => Some(format!(
            "/api/v1/p/icpc/api/plugins/icpc/contests/{contest_id}/standings"
        )),
        "ioi" => Some(format!(
            "/api/v1/p/ioi/api/plugins/ioi/contests/{contest_id}/scoreboard"
        )),
        _ => None,
    }
}

pub fn effective_rate(config: &MixedConfig, next_sequence: u64) -> u32 {
    if config.final_burst_duration.is_zero() {
        return config.rate;
    }

    let burst_ops = config.rate as u64 * config.final_burst_duration.as_secs().max(1);
    // Base the burst window on the real op count: run() iterates
    // 0..planned_operations(config), which in --duration mode far exceeds
    // config.total (they are equal only in --total mode).
    let total = planned_operations(config);
    let remaining = total.saturating_sub(next_sequence);
    if remaining <= burst_ops {
        config
            .rate
            .saturating_mul(config.final_burst_multiplier.max(1))
    } else {
        config.rate
    }
}

pub fn planned_operations(config: &MixedConfig) -> u64 {
    let Some(duration) = config.duration else {
        return config.total;
    };

    let duration_secs = duration.as_secs().max(1);
    let burst_secs = config.final_burst_duration.as_secs().min(duration_secs);
    let steady_secs = duration_secs - burst_secs;
    let steady_ops = steady_secs.saturating_mul(config.rate as u64);
    let burst_ops = burst_secs
        .saturating_mul(config.rate as u64)
        .saturating_mul(config.final_burst_multiplier.max(1) as u64);
    steady_ops.saturating_add(burst_ops).max(1)
}

pub fn frontend_asset_paths(contest_id: i32) -> Vec<String> {
    vec![
        "/".to_string(),
        format!("/contests/{contest_id}"),
        format!("/contests/{contest_id}/problems"),
        format!("/contests/{contest_id}/rankings"),
    ]
}

pub async fn run(
    client: &Client,
    state: &BootstrapState,
    scenarios: &'static [Scenario],
    config: &MixedConfig,
    tx: &mpsc::UnboundedSender<Event>,
) -> MixedOutcome {
    let _ = tx.send(Event::PhaseStarted {
        phase: Phase::Load,
        total: Some(planned_operations(config)),
    });

    let total = planned_operations(config);
    if total == 0 || config.rate == 0 || config.concurrency == 0 {
        let outcome = MixedOutcome::empty(total);
        let _ = tx.send(Event::PhaseFinished {
            phase: Phase::Load,
            ok: outcome.passed_overall,
        });
        return outcome;
    }

    let scenario_problem_pairs: Vec<(&'static Scenario, i32)> = scenarios
        .iter()
        .filter_map(|scenario| {
            state
                .problem_ids_by_scenario
                .get(scenario.id)
                .map(|problem_id| (scenario, *problem_id))
        })
        .collect();
    if scenario_problem_pairs.is_empty() {
        let mut outcome = MixedOutcome::empty(total);
        outcome.passed_overall = false;
        outcome
            .errors
            .push((0, "mixed profile has no scenario/problem pairs".to_string()));
        let _ = tx.send(Event::Error {
            phase: Some(Phase::Load),
            message: "mixed profile has no scenario/problem pairs".to_string(),
        });
        let _ = tx.send(Event::PhaseFinished {
            phase: Phase::Load,
            ok: false,
        });
        return outcome;
    }

    let clients = match build_contestant_clients(client, state, config).await {
        Ok(clients) => clients,
        Err(e) => {
            let mut outcome = MixedOutcome::empty(total);
            outcome.passed_overall = false;
            outcome.errors.push((0, format!("contestant setup: {e}")));
            let _ = tx.send(Event::Error {
                phase: Some(Phase::Load),
                message: format!("contestant setup: {e}"),
            });
            let _ = tx.send(Event::PhaseFinished {
                phase: Phase::Load,
                ok: false,
            });
            return outcome;
        }
    };
    let actions = build_action_mix(clients.iter().any(Client::supports_login_probe));
    let histogram = Arc::new(Mutex::new(
        Histogram::<u64>::new_with_bounds(HISTOGRAM_LOW, HISTOGRAM_HIGH, HISTOGRAM_SIGFIG)
            .expect("static histogram bounds are valid"),
    ));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let stats = Arc::new(Mutex::new(HashMap::<MixedAction, ActionStats>::new()));
    let completed = Arc::new(Mutex::new(0u64));
    let semaphore = Arc::new(Semaphore::new(config.concurrency as usize));
    let mut rng = StdRng::seed_from_u64(config.seed);
    let mut join_set = JoinSet::new();

    for sequence in 0..total {
        let rate = effective_rate(config, sequence).max(1);
        let tick = Duration::from_micros(1_000_000 / rate as u64);
        sleep(tick.max(Duration::from_micros(1))).await;

        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore is never closed");
        let action = actions[rng.random_range(0..actions.len())];
        let (scenario, problem_id) =
            scenario_problem_pairs[rng.random_range(0..scenario_problem_pairs.len())];
        let client = clients[rng.random_range(0..clients.len())].clone();
        let state = state.clone();
        let histogram = histogram.clone();
        let errors = errors.clone();
        let stats = stats.clone();
        let completed = completed.clone();
        let timeout = config.per_job_timeout;

        join_set.spawn(async move {
            let _permit = permit;
            let started = Instant::now();
            let result =
                execute_action(&client, &state, action, problem_id, scenario, timeout).await;
            let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

            {
                let mut h = histogram.lock().await;
                let _ = h.record(latency_ms.max(1));
            }
            {
                let mut by_action = stats.lock().await;
                let entry = by_action.entry(action).or_default();
                if result.is_ok() {
                    entry.ok += 1;
                } else {
                    entry.error += 1;
                }
            }
            {
                let mut c = completed.lock().await;
                *c += 1;
            }
            if let Err(e) = result {
                errors
                    .lock()
                    .await
                    .push((sequence, format!("{}: {}", action.label(), e)));
            }
        });
    }

    while join_set.join_next().await.is_some() {}

    let completed = *completed.lock().await;
    let errors_vec = errors.lock().await.clone();
    let passed_overall = completed == total && errors_vec.is_empty();
    let histogram = histogram.lock().await.clone();
    let by_action = stats.lock().await.clone();

    let _ = tx.send(Event::PhaseFinished {
        phase: Phase::Load,
        ok: passed_overall,
    });

    MixedOutcome {
        total,
        completed,
        histogram,
        errors: errors_vec,
        by_action,
        passed_overall,
    }
}

async fn execute_action(
    client: &Client,
    state: &BootstrapState,
    action: MixedAction,
    problem_id: i32,
    scenario: &'static Scenario,
    timeout: Duration,
) -> StressResult<()> {
    match action {
        MixedAction::FrontendAsset => {
            for path in frontend_asset_paths(state.contest_id) {
                let body = match client.get_public_bytes(&path).await {
                    Ok(body) => body,
                    Err(e) if is_not_found(&e) => continue,
                    Err(e) => return Err(e),
                };
                for asset_path in discover_asset_paths(&body) {
                    if let Err(e) = client.get_public_bytes(&asset_path).await
                        && !is_not_found(&e)
                    {
                        return Err(e);
                    }
                }
            }
            Ok(())
        }
        MixedAction::SessionLogin => client.login().await,
        MixedAction::ContestRead => {
            client.get_contest(state.contest_id).await?;
            client.list_contest_problems(state.contest_id).await?;
            Ok(())
        }
        MixedAction::ProblemRead => {
            client.get_problem(problem_id).await?;
            Ok(())
        }
        MixedAction::SampleRead => {
            client.get_problem(problem_id).await?;
            Ok(())
        }
        MixedAction::AttachmentRead => {
            let attachments = client.list_attachments(problem_id).await?;
            if let Some(attachment) = attachments.attachments.first() {
                let _ = client
                    .download_attachment(problem_id, &attachment.id)
                    .await?;
            }
            Ok(())
        }
        MixedAction::CodeRun => {
            let req = build_code_run_request(scenario);
            let code_run = client
                .run_contest_code(state.contest_id, problem_id, &req)
                .await?;
            poll_code_run_until_terminal(client, code_run.id, timeout).await?;
            Ok(())
        }
        MixedAction::OfficialSubmission => {
            let req = build_submission_request(scenario, &state.contest_type);
            let submission = client
                .create_contest_submission(state.contest_id, problem_id, &req)
                .await?;
            poll_submission_until_terminal(client, submission.id, timeout).await?;
            Ok(())
        }
        MixedAction::ScoreboardRead => {
            if let Some(path) = scoreboard_path(&state.contest_type, state.contest_id) {
                let _: serde_json::Value = client.get_json_path(&path).await?;
            } else {
                client.list_contest_problems(state.contest_id).await?;
            }
            Ok(())
        }
    }
}

async fn build_contestant_clients(
    client: &Client,
    state: &BootstrapState,
    config: &MixedConfig,
) -> StressResult<Vec<Client>> {
    if config.contestants == 0 {
        return Ok(vec![client.clone()]);
    }

    let entries: Vec<CreateUserEntry> = (0..config.contestants)
        .map(|idx| CreateUserEntry {
            username: contestant_username(client.run_id(), idx),
            display_name: None,
            password: Some(contestant_password(client.run_id(), idx)),
        })
        .collect();

    let response = client
        .bulk_add_participants(
            state.contest_id,
            &BulkAddParticipantsRequest {
                usernames: vec![],
                create_users: entries.clone(),
            },
        )
        .await?;

    if response.created.is_empty()
        && response.added.is_empty()
        && response.already_enrolled.is_empty()
    {
        return Err(StressError::Other(anyhow!(
            "bulk participant creation returned no created or enrolled users"
        )));
    }
    if !response.not_found.is_empty() {
        return Err(StressError::Other(anyhow!(
            "bulk participant creation reported missing users: {}",
            response.not_found.join(", ")
        )));
    }

    let mut clients = Vec::with_capacity(entries.len());
    for (idx, entry) in entries.into_iter().enumerate() {
        clients.push(
            Client::new_with_run_id(
                client_base_url_hint(client),
                crate::client::AuthCreds::UsernamePassword {
                    username: entry.username,
                    password: entry
                        .password
                        .expect("stress-test contestant passwords are always set"),
                },
                format!("{}-u{}", client.run_id(), idx),
            )
            .await?,
        );
    }
    Ok(clients)
}

fn client_base_url_hint(client: &Client) -> String {
    client.base_url().to_string()
}

fn contestant_username(run_id: &str, idx: u32) -> String {
    let sanitized: String = run_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(16)
        .collect();
    format!("st{}_{idx:04}", sanitized)
        .chars()
        .take(32)
        .collect()
}

fn contestant_password(run_id: &str, idx: u32) -> String {
    format!("BroccoliStress_{run_id}_{idx}")
}

fn is_not_found(error: &StressError) -> bool {
    matches!(error, StressError::Api { status: 404, .. })
}

fn discover_asset_paths(body: &[u8]) -> Vec<String> {
    let Ok(text) = std::str::from_utf8(body) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for marker in ["src=\"", "href=\""] {
        for part in text.split(marker).skip(1) {
            if let Some(path) = part.split('"').next()
                && path.starts_with("/assets/")
                && !out.iter().any(|existing| existing == path)
            {
                out.push(path.to_string());
            }
        }
    }
    out
}

fn build_submission_request(scenario: &Scenario, contest_type: &str) -> CreateSubmissionRequest {
    let files = scenario
        .files
        .iter()
        .map(|(filename, content)| SubmissionFileDto {
            filename: (*filename).to_string(),
            content: (*content).to_string(),
        })
        .collect();

    CreateSubmissionRequest {
        files,
        language: scenario.language.to_string(),
        contest_type: Some(contest_type.to_string()),
    }
}

fn build_code_run_request(scenario: &Scenario) -> RunCodeRequest {
    RunCodeRequest {
        files: scenario
            .files
            .iter()
            .map(|(filename, content)| SubmissionFileDto {
                filename: (*filename).to_string(),
                content: (*content).to_string(),
            })
            .collect(),
        language: scenario.language.to_string(),
        custom_test_cases: vec![CustomTestCaseInput {
            input: "1 2\n".to_string(),
            expected_output: Some("3\n".to_string()),
        }],
    }
}

async fn poll_submission_until_terminal(
    client: &Client,
    submission_id: i32,
    timeout: Duration,
) -> StressResult<SubmissionResponse> {
    let deadline = Instant::now() + timeout;
    loop {
        let resp = client.get_submission(submission_id).await?;
        if resp.status.is_terminal() {
            return Ok(resp);
        }
        if Instant::now() >= deadline {
            return Err(StressError::Other(anyhow!(
                "submission {} did not reach a terminal status within {:?}",
                submission_id,
                timeout,
            )));
        }
        sleep(POLL_INTERVAL).await;
    }
}

async fn poll_code_run_until_terminal(
    client: &Client,
    code_run_id: i32,
    timeout: Duration,
) -> StressResult<CodeRunResponse> {
    let deadline = Instant::now() + timeout;
    loop {
        let resp = client.get_code_run(code_run_id).await?;
        if resp.status.is_terminal() {
            return Ok(resp);
        }
        if Instant::now() >= deadline {
            return Err(StressError::Other(anyhow!(
                "code run {} did not reach a terminal status within {:?}",
                code_run_id,
                timeout,
            )));
        }
        sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weighted_actions_cover_contest_web_and_judge_paths() {
        let actions = build_action_mix(true);

        assert!(actions.contains(&MixedAction::SessionLogin));
        assert!(actions.contains(&MixedAction::FrontendAsset));
        assert!(actions.contains(&MixedAction::ContestRead));
        assert!(actions.contains(&MixedAction::ScoreboardRead));
        assert!(actions.contains(&MixedAction::ProblemRead));
        assert!(actions.contains(&MixedAction::SampleRead));
        assert!(actions.contains(&MixedAction::CodeRun));
        assert!(actions.contains(&MixedAction::OfficialSubmission));
        assert!(actions.contains(&MixedAction::AttachmentRead));
        assert_eq!(actions.len(), 100);
    }

    #[test]
    fn token_only_action_mix_excludes_login_probe() {
        let actions = build_action_mix(false);

        assert!(!actions.contains(&MixedAction::SessionLogin));
        assert_eq!(actions.len(), 100);
    }

    #[test]
    fn scoreboard_path_tracks_known_contest_plugins() {
        assert_eq!(
            scoreboard_path("ioi", 7),
            Some("/api/v1/p/ioi/api/plugins/ioi/contests/7/scoreboard".to_string())
        );
        assert_eq!(
            scoreboard_path("icpc", 7),
            Some("/api/v1/p/icpc/api/plugins/icpc/contests/7/standings".to_string())
        );
        assert_eq!(scoreboard_path("unknown", 7), None);
    }

    #[test]
    fn final_burst_increases_rate_near_end() {
        let config = MixedConfig {
            total: 100,
            duration: None,
            rate: 10,
            concurrency: 10,
            per_job_timeout: Duration::from_secs(30),
            seed: 1,
            contestants: 0,
            final_burst_duration: Duration::from_secs(2),
            final_burst_multiplier: 3,
        };

        assert_eq!(effective_rate(&config, 79), 10);
        assert_eq!(effective_rate(&config, 80), 30);
    }

    #[test]
    fn final_burst_window_uses_planned_ops_in_duration_mode() {
        // Regression: --duration mode with a small `total`. planned_operations is
        // 18*10 + 2*10*3 = 240; the boost must fire only for the last
        // rate*burst_secs = 20 ops (seq >= 220), NOT from config.total(100).
        let config = MixedConfig {
            total: 100,
            duration: Some(Duration::from_secs(20)),
            rate: 10,
            concurrency: 10,
            per_job_timeout: Duration::from_secs(30),
            seed: 1,
            contestants: 0,
            final_burst_duration: Duration::from_secs(2),
            final_burst_multiplier: 3,
        };

        assert_eq!(planned_operations(&config), 240);
        // Mid-run stays at the steady rate (pre-fix this saturated to the boost).
        assert_eq!(effective_rate(&config, 150), 10);
        assert_eq!(effective_rate(&config, 219), 10);
        // Tail boosts.
        assert_eq!(effective_rate(&config, 220), 30);
    }

    #[test]
    fn duration_sets_planned_operations_from_rate() {
        let config = MixedConfig {
            total: 999,
            duration: Some(Duration::from_secs(12)),
            rate: 5,
            concurrency: 10,
            per_job_timeout: Duration::from_secs(30),
            seed: 1,
            contestants: 0,
            final_burst_duration: Duration::ZERO,
            final_burst_multiplier: 3,
        };

        assert_eq!(planned_operations(&config), 60);
    }

    #[test]
    fn frontend_asset_paths_include_spa_routes() {
        assert_eq!(
            frontend_asset_paths(7),
            vec![
                "/".to_string(),
                "/contests/7".to_string(),
                "/contests/7/problems".to_string(),
                "/contests/7/rankings".to_string()
            ]
        );
    }
}
