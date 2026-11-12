use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::bootstrap::BootstrapState;
use crate::client::Client;
use crate::dto::{CreateSubmissionRequest, SubmissionFileDto, SubmissionStatus, Verdict};
use crate::events::{Event, Phase};
use crate::scenarios::Scenario;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectnessOutcome {
    pub total: usize,
    pub passed: usize,
    pub failed_scenarios: Vec<String>,
}

impl CorrectnessOutcome {
    pub fn is_ok(&self) -> bool {
        self.failed_scenarios.is_empty()
    }
}

pub async fn run(
    client: &Client,
    state: &BootstrapState,
    scenarios: &[Scenario],
    per_job_timeout: Duration,
    tx: &mpsc::UnboundedSender<Event>,
) -> CorrectnessOutcome {
    let _ = tx.send(Event::PhaseStarted {
        phase: Phase::Correctness,
        total: Some(scenarios.len() as u64),
    });

    let mut passed: usize = 0;
    let mut failed_scenarios: Vec<String> = Vec::new();

    for scenario in scenarios {
        let _ = tx.send(Event::ScenarioStarted {
            id: scenario.id.to_string(),
        });
        let started = Instant::now();

        let scenario_ok = run_scenario(client, state, scenario, per_job_timeout, started, tx).await;

        if scenario_ok {
            passed += 1;
        } else {
            failed_scenarios.push(scenario.id.to_string());
            break;
        }
    }

    let outcome = CorrectnessOutcome {
        total: scenarios.len(),
        passed,
        failed_scenarios,
    };

    let _ = tx.send(Event::PhaseFinished {
        phase: Phase::Correctness,
        ok: outcome.is_ok(),
    });
    outcome
}

async fn run_scenario(
    client: &Client,
    state: &BootstrapState,
    scenario: &Scenario,
    per_job_timeout: Duration,
    started: Instant,
    tx: &mpsc::UnboundedSender<Event>,
) -> bool {
    let problem_id = match state.problem_ids_by_scenario.get(scenario.id) {
        Some(id) => *id,
        None => {
            let _ = tx.send(Event::Error {
                phase: Some(Phase::Correctness),
                message: format!(
                    "scenario `{}` has no bootstrapped problem id; bug in caller",
                    scenario.id,
                ),
            });
            let _ = tx.send(Event::ScenarioFinished {
                id: scenario.id.to_string(),
                ok: false,
                status: SubmissionStatus::Pending,
                verdict: None,
                duration_ms: started.elapsed().as_millis() as u64,
            });
            return false;
        }
    };

    let req = build_submission_request(scenario, &state.contest_type);

    let submitted = match client
        .create_contest_submission(state.contest_id, problem_id, &req)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(Event::Error {
                phase: Some(Phase::Correctness),
                message: format!("scenario `{}`: submit failed: {}", scenario.id, e),
            });
            let _ = tx.send(Event::ScenarioFinished {
                id: scenario.id.to_string(),
                ok: false,
                status: SubmissionStatus::Pending,
                verdict: None,
                duration_ms: started.elapsed().as_millis() as u64,
            });
            return false;
        }
    };

    match client
        .wait_for_submission(submitted.id, per_job_timeout)
        .await
    {
        Ok(final_resp) => {
            let actual_status = final_resp.status;
            let actual_verdict = final_resp
                .result
                .as_ref()
                .and_then(|r| r.verdict.as_ref())
                .cloned();
            let ok = scenario_passes(scenario, actual_status, actual_verdict.as_ref());

            if !ok {
                warn!(
                    scenario_id = scenario.id,
                    expected_status = ?scenario.expected_status,
                    expected_verdict = ?scenario.expected_verdict,
                    actual_status = ?actual_status,
                    actual_verdict = ?actual_verdict,
                    "correctness scenario mismatch",
                );
            } else {
                info!(
                    scenario_id = scenario.id,
                    status = ?actual_status,
                    verdict = ?actual_verdict,
                    "correctness scenario passed",
                );
            }

            let _ = tx.send(Event::ScenarioFinished {
                id: scenario.id.to_string(),
                ok,
                status: actual_status,
                verdict: actual_verdict,
                duration_ms: started.elapsed().as_millis() as u64,
            });
            ok
        }
        Err(e) => {
            let _ = tx.send(Event::Error {
                phase: Some(Phase::Correctness),
                message: format!("scenario `{}`: poll failed: {}", scenario.id, e),
            });
            let _ = tx.send(Event::ScenarioFinished {
                id: scenario.id.to_string(),
                ok: false,
                status: SubmissionStatus::Pending,
                verdict: None,
                duration_ms: started.elapsed().as_millis() as u64,
            });
            false
        }
    }
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

fn scenario_passes(
    scenario: &Scenario,
    actual_status: SubmissionStatus,
    actual_verdict: Option<&Verdict>,
) -> bool {
    if actual_status == SubmissionStatus::SystemError {
        return false;
    }
    if actual_status != scenario.expected_status {
        return false;
    }
    if actual_status == SubmissionStatus::Judged {
        return actual_verdict == scenario.expected_verdict.as_ref();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::scenarios::SCENARIOS;
    use crate::test_support::*;

    use std::collections::HashMap;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TEST_CONTEST_ID: i32 = 9001;

    fn make_state() -> BootstrapState {
        BootstrapState {
            contest_type: "icpc".into(),
            problem_type: "batch".into(),
            contest_id: TEST_CONTEST_ID,
            problem_ids_by_scenario: SCENARIOS
                .iter()
                .enumerate()
                .map(|(i, s)| (s.id, 100 + i as i32))
                .collect(),
            owns_fixtures: true,
        }
    }

    async fn mount_per_scenario_mocks(
        server: &MockServer,
        state: &BootstrapState,
        outcomes: &HashMap<&'static str, (SubmissionStatus, Option<Verdict>)>,
    ) {
        for (idx, scenario) in SCENARIOS.iter().enumerate() {
            let problem_id = state.problem_ids_by_scenario[scenario.id];
            let submission_id = 1000 + idx as i32;
            let post_path = format!(
                "/api/v1/contests/{}/problems/{}/submissions",
                TEST_CONTEST_ID, problem_id,
            );
            let get_path = format!("/api/v1/submissions/{}", submission_id);

            Mock::given(method("POST"))
                .and(path(post_path.clone()))
                .respond_with(ResponseTemplate::new(201).set_body_json(submission_json(
                    submission_id,
                    SubmissionStatus::Pending,
                    None,
                )))
                .mount(server)
                .await;

            let (status, verdict) = outcomes
                .get(scenario.id)
                .cloned()
                .unwrap_or_else(|| (scenario.expected_status, scenario.expected_verdict.clone()));
            Mock::given(method("GET"))
                .and(path(get_path))
                .respond_with(ResponseTemplate::new(200).set_body_json(submission_json(
                    submission_id,
                    status,
                    verdict,
                )))
                .mount(server)
                .await;
        }
    }

    #[tokio::test]
    async fn passes_when_every_scenario_verdict_matches() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;
        let state = make_state();

        let outcomes = HashMap::new();
        mount_per_scenario_mocks(&server, &state, &outcomes).await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let outcome = run(&client, &state, SCENARIOS, Duration::from_secs(5), &tx).await;
        drop(tx);

        assert!(outcome.is_ok(), "all 9 scenarios should pass");
        assert_eq!(outcome.total, SCENARIOS.len());
        assert_eq!(outcome.passed, SCENARIOS.len());
        assert!(outcome.failed_scenarios.is_empty());

        let events = drain(&mut rx);
        assert_eq!(events.len(), 1 + 9 * 2 + 1, "events: {:#?}", events);

        match &events[0] {
            Event::PhaseStarted { phase, .. } => assert_eq!(*phase, Phase::Correctness),
            other => panic!("expected PhaseStarted, got {:?}", other),
        }
        match events.last().unwrap() {
            Event::PhaseFinished { phase, ok } => {
                assert_eq!(*phase, Phase::Correctness);
                assert!(*ok);
            }
            other => panic!("expected PhaseFinished, got {:?}", other),
        }

        let finished: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::ScenarioFinished { id, ok, .. } => Some((id.clone(), *ok)),
                _ => None,
            })
            .collect();
        assert_eq!(finished.len(), 9);
        for (i, (id, ok)) in finished.iter().enumerate() {
            assert!(*ok, "scenario {} ({}) should pass", i, id);
            assert_eq!(id.as_str(), SCENARIOS[i].id);
        }
    }

    #[tokio::test]
    async fn fails_fast_on_first_mismatch() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;
        let state = make_state();

        let target_scenario = &SCENARIOS[2];
        let bad_verdict = if target_scenario.expected_verdict == Some(Verdict::WrongAnswer) {
            Verdict::Accepted
        } else {
            Verdict::WrongAnswer
        };

        let mut outcomes = HashMap::new();
        outcomes.insert(
            target_scenario.id,
            (SubmissionStatus::Judged, Some(bad_verdict)),
        );
        mount_per_scenario_mocks(&server, &state, &outcomes).await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let outcome = run(&client, &state, SCENARIOS, Duration::from_secs(5), &tx).await;
        drop(tx);

        assert!(!outcome.is_ok(), "phase must fail");
        assert_eq!(outcome.total, SCENARIOS.len());
        assert_eq!(outcome.passed, 2, "first two scenarios passed before break");
        assert_eq!(
            outcome.failed_scenarios,
            vec![target_scenario.id.to_string()],
        );

        let events = drain(&mut rx);

        assert_eq!(events.len(), 1 + 3 * 2 + 1, "events: {:#?}", events);

        let started: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::ScenarioStarted { id } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(started.len(), 3, "exactly 3 scenarios should have started");
        assert_eq!(started[0], SCENARIOS[0].id);
        assert_eq!(started[1], SCENARIOS[1].id);
        assert_eq!(started[2], SCENARIOS[2].id);

        let finished: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::ScenarioFinished { id, ok, .. } => Some((id.clone(), *ok)),
                _ => None,
            })
            .collect();
        assert_eq!(finished.len(), 3);
        assert!(finished[0].1, "scenario 0 ok");
        assert!(finished[1].1, "scenario 1 ok");
        assert!(!finished[2].1, "scenario 2 must fail");

        match events.last().unwrap() {
            Event::PhaseFinished { ok, .. } => assert!(!ok),
            other => panic!("expected PhaseFinished, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn system_error_is_always_a_failure() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;

        let scenario = &SCENARIOS[0];
        assert_eq!(scenario.expected_status, SubmissionStatus::Judged);

        let state = BootstrapState {
            contest_type: "icpc".into(),
            problem_type: "batch".into(),
            contest_id: 555,
            problem_ids_by_scenario: [(scenario.id, 200)].into_iter().collect(),
            owns_fixtures: true,
        };

        Mock::given(method("POST"))
            .and(path("/api/v1/contests/555/problems/200/submissions"))
            .respond_with(ResponseTemplate::new(201).set_body_json(submission_json(
                7777,
                SubmissionStatus::Pending,
                None,
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/submissions/7777"))
            .respond_with(ResponseTemplate::new(200).set_body_json(submission_json(
                7777,
                SubmissionStatus::SystemError,
                None,
            )))
            .mount(&server)
            .await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let outcome = run(
            &client,
            &state,
            std::slice::from_ref(scenario),
            Duration::from_secs(5),
            &tx,
        )
        .await;
        drop(tx);

        assert!(!outcome.is_ok(), "SystemError must always fail");
        assert_eq!(outcome.passed, 0);
        assert_eq!(outcome.failed_scenarios, vec![scenario.id.to_string()]);

        let events = drain(&mut rx);
        let finished = events
            .iter()
            .find_map(|e| match e {
                Event::ScenarioFinished { id, ok, status, .. } if id == scenario.id => {
                    Some((*ok, *status))
                }
                _ => None,
            })
            .expect("ScenarioFinished present");
        assert!(!finished.0);
        assert_eq!(finished.1, SubmissionStatus::SystemError);
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_treated_as_failure() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;

        let scenario = &SCENARIOS[0];
        let state = BootstrapState {
            contest_type: "icpc".into(),
            problem_type: "batch".into(),
            contest_id: 555,
            problem_ids_by_scenario: [(scenario.id, 300)].into_iter().collect(),
            owns_fixtures: true,
        };

        Mock::given(method("POST"))
            .and(path("/api/v1/contests/555/problems/300/submissions"))
            .respond_with(ResponseTemplate::new(201).set_body_json(submission_json(
                42,
                SubmissionStatus::Pending,
                None,
            )))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/v1/submissions/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(submission_json(
                42,
                SubmissionStatus::Running,
                None,
            )))
            .mount(&server)
            .await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let outcome = run(
            &client,
            &state,
            std::slice::from_ref(scenario),
            Duration::from_millis(500),
            &tx,
        )
        .await;
        drop(tx);

        assert!(!outcome.is_ok(), "timeout must fail the phase");
        assert_eq!(outcome.passed, 0);
        assert_eq!(outcome.failed_scenarios, vec![scenario.id.to_string()]);

        let events = drain(&mut rx);

        let finished: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::ScenarioFinished { id, ok, .. } => Some((id.clone(), *ok)),
                _ => None,
            })
            .collect();
        assert_eq!(finished.len(), 1);
        assert!(!finished[0].1);

        let error_seen = events.iter().any(|e| {
            matches!(
                e,
                Event::Error {
                    phase: Some(Phase::Correctness),
                    ..
                }
            )
        });
        assert!(error_seen, "must emit Error event on timeout");
    }

    #[tokio::test]
    async fn outcome_reports_actual_failed_scenario_id() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;
        let state = make_state();

        let target = &SCENARIOS[5];
        let bad_verdict = if target.expected_verdict == Some(Verdict::Accepted) {
            Verdict::WrongAnswer
        } else {
            Verdict::Accepted
        };

        let mut outcomes = HashMap::new();
        outcomes.insert(target.id, (SubmissionStatus::Judged, Some(bad_verdict)));
        mount_per_scenario_mocks(&server, &state, &outcomes).await;

        let (tx, _rx) = mpsc::unbounded_channel();
        let outcome = run(&client, &state, SCENARIOS, Duration::from_secs(5), &tx).await;

        assert_eq!(outcome.total, SCENARIOS.len());
        assert_eq!(outcome.passed, 5);
        assert_eq!(outcome.failed_scenarios, vec![target.id.to_string()]);
        assert!(!outcome.is_ok());
    }
}
