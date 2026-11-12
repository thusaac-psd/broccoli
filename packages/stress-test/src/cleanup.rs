use crate::bootstrap::BootstrapState;
use crate::client::Client;
use crate::error::StressError;

#[derive(Debug, Default)]
pub struct CleanupOutcome {
    pub deleted: usize,
    pub warnings: Vec<String>,
}

impl CleanupOutcome {
    pub fn is_clean(&self) -> bool {
        self.warnings.is_empty()
    }
}

pub async fn run(client: &Client, state: &BootstrapState) -> CleanupOutcome {
    let mut outcome = CleanupOutcome::default();

    if !state.owns_fixtures {
        return outcome;
    }

    match client.delete_contest(state.contest_id).await {
        Ok(()) => {}
        Err(StressError::Api { status: 404, .. }) => {}
        Err(e) => {
            outcome.warnings.push(format!(
                "could not delete scratch contest {}: {}",
                state.contest_id, e
            ));
        }
    }

    for (scenario_id, problem_id) in &state.problem_ids_by_scenario {
        match client.delete_problem(*problem_id).await {
            Ok(()) => {
                outcome.deleted += 1;
            }
            Err(StressError::Api { status: 404, .. }) => {
                outcome.deleted += 1;
            }
            Err(e) => {
                outcome.warnings.push(format!(
                    "could not delete problem {} (scenario {}): {}",
                    problem_id, scenario_id, e
                ));
            }
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::client_with_login;

    use serde_json::json;
    use std::collections::HashMap;
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TEST_CONTEST_ID: i32 = 9999;

    async fn mount_default_contest_delete(server: &MockServer) {
        Mock::given(method("DELETE"))
            .and(path(format!("/api/v1/contests/{TEST_CONTEST_ID}")))
            .respond_with(ResponseTemplate::new(204))
            .mount(server)
            .await;
    }

    fn state_with(ids: &[(&'static str, i32)]) -> BootstrapState {
        let mut map = HashMap::new();
        for (k, v) in ids {
            map.insert(*k, *v);
        }
        BootstrapState {
            contest_type: "icpc".into(),
            problem_type: "batch".into(),
            contest_id: TEST_CONTEST_ID,
            problem_ids_by_scenario: map,
            owns_fixtures: true,
        }
    }

    #[tokio::test]
    async fn happy_path_deletes_every_problem() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;
        mount_default_contest_delete(&server).await;

        Mock::given(method("DELETE"))
            .and(path_regex(r"^/api/v1/problems/\d+$"))
            .respond_with(ResponseTemplate::new(204))
            .expect(3)
            .mount(&server)
            .await;

        let state = state_with(&[("a", 10), ("b", 20), ("c", 30)]);
        let outcome = run(&client, &state).await;

        assert_eq!(outcome.deleted, 3);
        assert!(outcome.warnings.is_empty());
        assert!(outcome.is_clean());
    }

    #[tokio::test]
    async fn not_found_counts_as_already_clean_no_warning() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;
        mount_default_contest_delete(&server).await;

        Mock::given(method("DELETE"))
            .and(path("/api/v1/problems/10"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "code": "NOT_FOUND",
                "message": "problem 10 not found",
            })))
            .mount(&server)
            .await;

        let state = state_with(&[("a", 10)]);
        let outcome = run(&client, &state).await;

        assert_eq!(outcome.deleted, 1);
        assert!(
            outcome.warnings.is_empty(),
            "404 must not generate a warning"
        );
        assert!(outcome.is_clean());
    }

    #[tokio::test]
    async fn server_error_recorded_as_warning_not_panic() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;
        mount_default_contest_delete(&server).await;

        Mock::given(method("DELETE"))
            .and(path("/api/v1/problems/10"))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({
                "code": "INTERNAL_ERROR",
                "message": "boom",
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/problems/20"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let state = state_with(&[("a", 10), ("b", 20)]);
        let outcome = run(&client, &state).await;

        assert_eq!(outcome.deleted, 1, "20 deleted; 10 failed");
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.warnings[0].contains("problem 10"));
        assert!(outcome.warnings[0].contains("scenario a"));
        assert!(!outcome.is_clean());
    }

    #[tokio::test]
    async fn empty_state_returns_clean_outcome() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;
        mount_default_contest_delete(&server).await;

        let state = state_with(&[]);
        let outcome = run(&client, &state).await;

        assert_eq!(outcome.deleted, 0);
        assert!(outcome.is_clean());
    }

    #[tokio::test]
    async fn contest_delete_404_is_silent() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;

        Mock::given(method("DELETE"))
            .and(path(format!("/api/v1/contests/{TEST_CONTEST_ID}")))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "code": "NOT_FOUND",
                "message": "contest not found",
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(r"^/api/v1/problems/\d+$"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let state = state_with(&[("a", 10)]);
        let outcome = run(&client, &state).await;

        assert!(outcome.warnings.is_empty(), "404 must not warn");
        assert_eq!(outcome.deleted, 1);
    }

    #[tokio::test]
    async fn contest_delete_500_warns_but_continues_to_delete_problems() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;

        Mock::given(method("DELETE"))
            .and(path(format!("/api/v1/contests/{TEST_CONTEST_ID}")))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({
                "code": "INTERNAL_ERROR",
                "message": "boom",
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/problems/10"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let state = state_with(&[("a", 10)]);
        let outcome = run(&client, &state).await;

        assert_eq!(outcome.deleted, 1, "problems still get deleted");
        assert_eq!(outcome.warnings.len(), 1);
        assert!(
            outcome.warnings[0].contains("scratch contest"),
            "warning must mention contest, got: {}",
            outcome.warnings[0]
        );
    }

    #[tokio::test]
    async fn existing_contest_state_is_not_deleted() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;

        let mut state = state_with(&[("a", 10)]);
        state.owns_fixtures = false;
        let outcome = run(&client, &state).await;

        assert_eq!(outcome.deleted, 0);
        assert!(outcome.warnings.is_empty());
        assert!(outcome.is_clean());
    }
}
