use std::collections::HashMap;

use tracing::info;

use crate::client::Client;
use crate::dto::{
    AddContestProblemRequest, CreateContestRequest, CreateProblemRequest, CreateTestCaseRequest,
    RegistriesResponse,
};
use crate::error::{StressError, StressResult};
use crate::scenarios::Scenario;

#[derive(Debug, Clone)]
pub struct BootstrapState {
    pub contest_type: String,
    pub problem_type: String,
    pub contest_id: i32,
    pub problem_ids_by_scenario: HashMap<&'static str, i32>,
    pub owns_fixtures: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BootstrapConfig {
    pub contest_type: Option<String>,
    pub problem_type: Option<String>,
}

pub async fn bootstrap(
    client: &Client,
    scenarios: &[Scenario],
    config: &BootstrapConfig,
) -> StressResult<BootstrapState> {
    let registries = client.list_registries().await?;
    let contest_type = resolve_contest_type(&registries, config.contest_type.as_deref())?;
    let problem_type = resolve_problem_type(&registries, config.problem_type.as_deref())?;

    info!(
        contest_type = %contest_type,
        problem_type = %problem_type,
        "stress-test bootstrap resolved registry types",
    );

    let contest = client
        .create_contest(&build_create_contest_request(&contest_type))
        .await?;
    info!(contest_id = contest.id, "bootstrap created scratch contest",);

    let mut problem_ids_by_scenario = HashMap::with_capacity(scenarios.len());

    for (idx, scenario) in scenarios.iter().enumerate() {
        let req = build_create_problem_request(scenario, &contest_type, &problem_type);
        let problem = client.create_problem(&req).await?;

        let tc_req = CreateTestCaseRequest {
            input: scenario.test_input.to_string(),
            expected_output: scenario.test_expected_output.to_string(),
            score: 100,
            is_sample: false,
            position: None,
            description: None,
            label: None,
        };
        client.create_test_case(problem.id, &tc_req).await?;

        let attach_req = AddContestProblemRequest {
            problem_id: problem.id,
            label: contest_label(idx),
            position: None,
        };
        client
            .add_problem_to_contest(contest.id, &attach_req)
            .await?;

        info!(
            scenario_id = scenario.id,
            problem_id = problem.id,
            "bootstrap created problem + test case",
        );
        problem_ids_by_scenario.insert(scenario.id, problem.id);
    }

    Ok(BootstrapState {
        contest_type,
        problem_type,
        contest_id: contest.id,
        problem_ids_by_scenario,
        owns_fixtures: true,
    })
}

pub async fn load_existing_contest(
    client: &Client,
    contest_id: i32,
    problem_id: Option<i32>,
    scenarios: &[Scenario],
    config: &BootstrapConfig,
) -> StressResult<BootstrapState> {
    let contest = client.get_contest(contest_id).await?;
    let registries = if config.contest_type.is_some()
        || contest.contest_type.is_none()
        || config.problem_type.is_some()
    {
        Some(client.list_registries().await?)
    } else {
        None
    };
    let contest_type = match config.contest_type.as_deref() {
        Some(override_value) => resolve_contest_type(
            registries
                .as_ref()
                .expect("registries loaded when contest type override is set"),
            Some(override_value),
        )?,
        None => match contest.contest_type.clone() {
            Some(contest_type) => contest_type,
            None => resolve_contest_type(
                registries
                    .as_ref()
                    .expect("registries loaded when contest type is absent"),
                None,
            )?,
        },
    };
    let problem_type = match config.problem_type.as_deref() {
        Some(override_value) => resolve_problem_type(
            registries
                .as_ref()
                .expect("registries loaded when problem type override is set"),
            Some(override_value),
        )?,
        None => "existing".to_string(),
    };

    let mut problems = client.list_contest_problems(contest_id).await?;
    if let Some(problem_id) = problem_id {
        problems.retain(|p| p.problem_id == problem_id);
    }
    if problems.is_empty() {
        return Err(StressError::Other(anyhow::anyhow!(
            "contest {contest_id} has no attached problems matching the requested target"
        )));
    }

    let mut problem_ids_by_scenario = HashMap::with_capacity(scenarios.len());
    for (idx, scenario) in scenarios.iter().enumerate() {
        let problem = &problems[idx % problems.len()];
        problem_ids_by_scenario.insert(scenario.id, problem.problem_id);
    }

    info!(
        contest_id,
        contest_type = %contest_type,
        problem_count = problems.len(),
        "stress-test loaded existing contest target",
    );

    Ok(BootstrapState {
        contest_type,
        problem_type,
        contest_id,
        problem_ids_by_scenario,
        owns_fixtures: false,
    })
}

pub(crate) fn build_create_contest_request(contest_type: &str) -> CreateContestRequest {
    let now = chrono::Utc::now();
    let suffix = now.timestamp();
    CreateContestRequest {
        title: format!("stress-test-{suffix}"),
        description: "Auto-created by broccoli-stress-test. \
                      Will be deleted after the run unless --keep-fixtures is set."
            .into(),
        activate_time: Some(Some(now - chrono::Duration::seconds(60))),
        start_time: now - chrono::Duration::seconds(60),
        end_time: now + chrono::Duration::hours(24),
        deactivate_time: Some(Some(now + chrono::Duration::hours(24))),
        is_public: false,
        submissions_visible: None,
        show_compile_output: None,
        show_participants_list: None,
        contest_type: contest_type.to_string(),
    }
}

pub(crate) fn contest_label(index: usize) -> String {
    const ALPHA: usize = (b'Z' - b'A' + 1) as usize;
    if index < ALPHA {
        ((b'A' + index as u8) as char).to_string()
    } else {
        format!("X{index}")
    }
}

fn resolve_contest_type(
    registries: &RegistriesResponse,
    override_value: Option<&str>,
) -> StressResult<String> {
    resolve_type(
        "contest_type",
        &registries.contest_types,
        override_value,
        "icpc",
        empty_contest_types_hint,
    )
}

pub(crate) fn resolve_problem_type(
    registries: &RegistriesResponse,
    override_value: Option<&str>,
) -> StressResult<String> {
    resolve_type(
        "problem_type",
        &registries.problem_types,
        override_value,
        "batch",
        empty_problem_types_hint,
    )
}

/// Pick the type to bootstrap with: the override if given, else `preferred`
/// (the type the A+B fixtures and their expected verdicts are written for),
/// else the only registered type. With several registered and no preferred
/// one, ask for an override instead of guessing: a format such as a
/// tournament bracket rejects ordinary submissions, so "the first one listed"
/// can make every submission fail.
fn resolve_type(
    label: &str,
    available: &[String],
    override_value: Option<&str>,
    preferred: &str,
    empty_hint: fn() -> &'static str,
) -> StressResult<String> {
    if let Some(requested) = override_value {
        if available.iter().any(|t| t == requested) {
            return Ok(requested.to_string());
        }
        return Err(StressError::Other(anyhow::anyhow!(
            "{label} override `{requested}` is not registered on the server. \
             Available {label}s: [{}]. {}",
            available.join(", "),
            empty_hint(),
        )));
    }

    if available.iter().any(|t| t == preferred) {
        return Ok(preferred.to_string());
    }
    if let [only] = available {
        return Ok(only.clone());
    }
    if !available.is_empty() {
        return Err(StressError::Other(anyhow::anyhow!(
            "`{preferred}` is not registered and several {label}s are; pass --{} to choose one of: [{}]",
            label.replace('_', "-"),
            available.join(", "),
        )));
    }

    Err(StressError::Other(anyhow::anyhow!(
        "no {label}s registered on the server. {}",
        empty_hint(),
    )))
}

fn empty_contest_types_hint() -> &'static str {
    "Load a contest plugin (e.g. `icpc` or `ioi`) plus `batch-evaluator` so \
     the server's plugin registries are populated, then re-run the stress test."
}

fn empty_problem_types_hint() -> &'static str {
    "Load `batch-evaluator` (and a contest plugin such as `icpc`) so the \
     server's plugin registries are populated, then re-run the stress test."
}

pub(crate) fn build_create_problem_request(
    scenario: &Scenario,
    contest_type: &str,
    problem_type: &str,
) -> CreateProblemRequest {
    let mut submission_format = HashMap::new();
    submission_format.insert(
        scenario.language.to_string(),
        scenario
            .files
            .iter()
            .map(|(name, _)| name.to_string())
            .collect(),
    );

    CreateProblemRequest {
        title: format!("stress-test:{}", scenario.id),
        content: format!(
            "Auto-created by broccoli-stress-test for scenario `{}`. \n\
             This problem will be deleted after the run unless --keep-fixtures is set.",
            scenario.id,
        ),
        time_limit: scenario.time_limit_ms,
        memory_limit: scenario.memory_limit_kb,
        problem_type: problem_type.to_string(),
        checker_format: scenario.checker_format.to_string(),
        default_contest_type: contest_type.to_string(),
        show_test_details: None,
        is_public: None,
        submission_format: Some(submission_format),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{client_with_login, contest_json, problem_json, test_case_json};

    fn types(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn hint() -> &'static str {
        "hint"
    }

    #[test]
    fn the_fixture_type_wins_over_whatever_sorts_first() {
        let available = types(&["codelink-bracket", "codelink-qualifier", "icpc", "ioi"]);
        let picked = resolve_type("contest_type", &available, None, "icpc", hint).unwrap();
        assert_eq!(picked, "icpc");
    }

    #[test]
    fn a_single_registered_type_is_used() {
        let picked = resolve_type("contest_type", &types(&["ioi"]), None, "icpc", hint).unwrap();
        assert_eq!(picked, "ioi");
    }

    #[test]
    fn several_types_without_the_fixture_type_need_an_override() {
        let err = resolve_type(
            "contest_type",
            &types(&["codelink-bracket", "ioi"]),
            None,
            "icpc",
            hint,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--contest-type"), "{err}");
    }

    #[test]
    fn problem_type_prefers_batch_over_the_first_registered_type() {
        let registries = RegistriesResponse {
            problem_types: types(&["communication", "batch"]),
            checker_formats: vec![],
            contest_types: vec![],
            languages: vec![],
            evaluators: vec![],
            contest_type_handlers: vec![],
            hooks: vec![],
        };
        assert_eq!(resolve_problem_type(&registries, None).unwrap(), "batch");
    }

    #[test]
    fn ambiguous_problem_types_without_batch_need_an_override() {
        let registries = RegistriesResponse {
            problem_types: types(&["communication", "interactive"]),
            checker_formats: vec![],
            contest_types: vec![],
            languages: vec![],
            evaluators: vec![],
            contest_type_handlers: vec![],
            hooks: vec![],
        };
        let error = resolve_problem_type(&registries, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--problem-type"), "{error}");
    }

    use crate::scenarios::SCENARIOS;
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    #[derive(Default)]
    struct CapturingMatcher {
        captured: std::sync::Mutex<Vec<Value>>,
    }

    impl CapturingMatcher {
        fn snapshot(&self) -> Vec<Value> {
            self.captured.lock().unwrap().clone()
        }
    }

    impl wiremock::Match for &CapturingMatcher {
        fn matches(&self, req: &Request) -> bool {
            if let Ok(parsed) = serde_json::from_slice::<Value>(&req.body) {
                self.captured.lock().unwrap().push(parsed);
            }
            true
        }
    }

    async fn mount_problem_creation_with_capture(
        server: &MockServer,
        capture: &'static CapturingMatcher,
    ) {
        Mock::given(method("POST"))
            .and(path("/api/v1/problems"))
            .and(capture)
            .respond_with(|req: &Request| {
                let body_len = req.body.len() as i32;
                let id = 1000 + body_len;
                let parsed: Value = serde_json::from_slice(&req.body).unwrap();
                let problem_type = parsed
                    .get("problem_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("batch")
                    .to_string();
                let contest_type = parsed
                    .get("default_contest_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("icpc")
                    .to_string();
                ResponseTemplate::new(201).set_body_json(problem_json(
                    id,
                    &problem_type,
                    "exact",
                    &contest_type,
                ))
            })
            .expect(9)
            .mount(server)
            .await;
    }

    async fn mount_test_case_creation(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path_regex(r"^/api/v1/problems/\d+/test-cases$"))
            .respond_with(ResponseTemplate::new(201).set_body_json(test_case_json(1, 1, "1\n")))
            .expect(9)
            .mount(server)
            .await;
    }

    async fn mount_contest_creation(server: &MockServer, contest_id: i32, contest_type: &str) {
        Mock::given(method("POST"))
            .and(path("/api/v1/contests"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(contest_json(contest_id, Some(contest_type))),
            )
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_contest_problem_attach(server: &MockServer, contest_id: i32) {
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/contests/{contest_id}/problems")))
            .respond_with(|req: &Request| {
                let parsed: Value = serde_json::from_slice(&req.body).unwrap();
                let pid = parsed
                    .get("problem_id")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32;
                let label = parsed
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("A")
                    .to_string();
                ResponseTemplate::new(201).set_body_json(json!({
                    "contest_id": 9001,
                    "problem_id": pid,
                    "label": label,
                    "position": 0,
                    "problem_title": "stress-test:x",
                }))
            })
            .expect(9)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn happy_path_creates_one_problem_and_one_test_case_per_scenario() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "t").await;

        Mock::given(method("GET"))
            .and(path("/api/v1/plugins/registries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "problem_types": ["batch"],
                "checker_formats": ["exact", "tokens-case-insensitive"],
                "contest_types": ["icpc"],
                "languages": [],
                "evaluators": [],
                "contest_type_handlers": [],
                "hooks": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        mount_contest_creation(&server, 9001, "icpc").await;
        mount_contest_problem_attach(&server, 9001).await;
        let capture: &'static CapturingMatcher = Box::leak(Box::default());
        mount_problem_creation_with_capture(&server, capture).await;
        mount_test_case_creation(&server).await;

        let state = bootstrap(&client, SCENARIOS, &BootstrapConfig::default())
            .await
            .expect("bootstrap succeeds");

        assert_eq!(state.contest_type, "icpc");
        assert_eq!(state.problem_type, "batch");
        assert_eq!(state.contest_id, 9001);
        assert!(state.owns_fixtures);
        assert_eq!(state.problem_ids_by_scenario.len(), 9);
        for s in SCENARIOS {
            assert!(
                state.problem_ids_by_scenario.contains_key(s.id),
                "missing problem id for scenario `{}`",
                s.id
            );
        }

        let captured = capture.snapshot();
        assert_eq!(captured.len(), 9);

        for body in &captured {
            let title = body.get("title").and_then(|v| v.as_str()).unwrap();
            let scenario = SCENARIOS
                .iter()
                .find(|s| title == format!("stress-test:{}", s.id))
                .expect("captured request matches a known scenario");

            let fmt = body
                .get("submission_format")
                .expect("submission_format must be present");
            assert!(!fmt.is_null(), "submission_format must not be null");
            let lang_files = fmt
                .get(scenario.language)
                .unwrap_or_else(|| panic!("missing language `{}` for {}", scenario.language, title))
                .as_array()
                .expect("language entry must be array");
            let want_filenames: Vec<&str> = scenario.files.iter().map(|(n, _)| *n).collect();
            let got_filenames: Vec<&str> = lang_files.iter().map(|v| v.as_str().unwrap()).collect();
            assert_eq!(
                got_filenames, want_filenames,
                "filenames for scenario {}",
                scenario.id
            );

            assert_eq!(
                body.get("default_contest_type").unwrap().as_str().unwrap(),
                "icpc"
            );
            assert_eq!(body.get("problem_type").unwrap().as_str().unwrap(), "batch");
            assert_eq!(
                body.get("checker_format").unwrap().as_str().unwrap(),
                scenario.checker_format
            );
        }
    }

    #[tokio::test]
    async fn cli_override_wins_over_first_entry_when_valid() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "t").await;

        Mock::given(method("GET"))
            .and(path("/api/v1/plugins/registries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "problem_types": ["batch", "interactive"],
                "checker_formats": ["exact"],
                "contest_types": ["icpc", "ioi"],
                "languages": [],
                "evaluators": [],
                "contest_type_handlers": [],
                "hooks": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        mount_contest_creation(&server, 9002, "ioi").await;
        mount_contest_problem_attach(&server, 9002).await;
        let capture: &'static CapturingMatcher = Box::leak(Box::default());
        mount_problem_creation_with_capture(&server, capture).await;
        mount_test_case_creation(&server).await;

        let state = bootstrap(
            &client,
            SCENARIOS,
            &BootstrapConfig {
                contest_type: Some("ioi".into()),
                problem_type: None,
            },
        )
        .await
        .expect("bootstrap succeeds with valid override");

        assert_eq!(state.contest_type, "ioi");
        assert_eq!(state.problem_type, "batch");
        assert_eq!(state.contest_id, 9002);
        assert!(state.owns_fixtures);

        for body in capture.snapshot() {
            assert_eq!(
                body.get("default_contest_type").unwrap().as_str().unwrap(),
                "ioi"
            );
        }
    }

    #[tokio::test]
    async fn override_not_in_registries_fails_with_helpful_message() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "t").await;

        Mock::given(method("GET"))
            .and(path("/api/v1/plugins/registries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "problem_types": ["batch"],
                "checker_formats": ["exact"],
                "contest_types": ["icpc"],
                "languages": [],
                "evaluators": [],
                "contest_type_handlers": [],
                "hooks": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let err = bootstrap(
            &client,
            SCENARIOS,
            &BootstrapConfig {
                contest_type: Some("nonsense".into()),
                problem_type: None,
            },
        )
        .await
        .expect_err("must fail on unknown override");

        let msg = format!("{}", err);
        assert!(
            msg.contains("nonsense"),
            "error must name the bad override; got: {msg}"
        );
        assert!(
            msg.contains("icpc"),
            "error must list available types; got: {msg}"
        );
    }

    #[tokio::test]
    async fn existing_contest_loads_attached_problem_ids_without_owning_fixtures() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "t").await;

        Mock::given(method("GET"))
            .and(path("/api/v1/contests/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(contest_json(42, Some("icpc"))))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/contests/42/problems"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "contest_id": 42,
                    "problem_id": 101,
                    "label": "A",
                    "position": 0,
                    "problem_title": "signpost"
                },
                {
                    "contest_id": 42,
                    "problem_id": 102,
                    "label": "B",
                    "position": 1,
                    "problem_title": "other"
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let state = load_existing_contest(
            &client,
            42,
            Some(101),
            SCENARIOS,
            &BootstrapConfig::default(),
        )
        .await
        .expect("existing contest loads");

        assert_eq!(state.contest_id, 42);
        assert_eq!(state.contest_type, "icpc");
        assert_eq!(state.problem_type, "existing");
        assert!(!state.owns_fixtures);
        assert_eq!(state.problem_ids_by_scenario.len(), SCENARIOS.len());
        assert!(state.problem_ids_by_scenario.values().all(|id| *id == 101));
    }

    #[test]
    fn contest_label_uses_uppercase_letters_and_falls_back_for_overflow() {
        assert_eq!(contest_label(0), "A");
        assert_eq!(contest_label(1), "B");
        assert_eq!(contest_label(8), "I");
        assert_eq!(contest_label(25), "Z");
        assert_eq!(contest_label(26), "X26");
        assert_eq!(contest_label(99), "X99");
    }

    #[tokio::test]
    async fn empty_registries_fails_fast_with_canonical_fix_hint() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "t").await;

        Mock::given(method("GET"))
            .and(path("/api/v1/plugins/registries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "problem_types": [],
                "checker_formats": [],
                "contest_types": [],
                "languages": [],
                "evaluators": [],
                "contest_type_handlers": [],
                "hooks": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let err = bootstrap(&client, SCENARIOS, &BootstrapConfig::default())
            .await
            .expect_err("must fail on empty registries");

        let msg = format!("{}", err);
        assert!(
            msg.contains("batch-evaluator"),
            "error message must name `batch-evaluator` as the canonical fix; got: {msg}"
        );
        assert!(
            msg.contains("contest_type") || msg.contains("contest"),
            "error message must explain which type is missing; got: {msg}"
        );
    }
}
