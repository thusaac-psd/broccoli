pub mod reservation;

#[cfg(target_arch = "wasm32")]
mod plugin {
    use broccoli_server_sdk::prelude::*;
    use broccoli_server_sdk::types::ConfigSource;
    use extism_pdk::{FnResult, plugin_fn};
    use serde::{Deserialize, Serialize};

    use crate::reservation::{self, LimitKey};

    #[derive(Debug, Default, Deserialize)]
    struct LimitsConfig {
        #[serde(default)]
        max_submissions: Option<u32>,
    }

    #[derive(Deserialize)]
    struct SubmissionCount {
        count: i64,
    }

    /// Schema bootstrap, invoked by the host on plugin load.
    #[plugin_fn]
    pub fn init() -> FnResult<String> {
        let host = Host::new();
        host.db
            .ensure_schema("submission-limit", &[reservation::SCHEMA_SQL])?;
        Ok("ok".into())
    }

    /// Take a slot for the attempt. Ok(false) when the cap is reached.
    fn try_claim(
        host: &Host,
        key: &LimitKey,
        event: &BeforeSubmissionEvent,
        max: u32,
    ) -> Result<bool, SdkError> {
        let mut p = Params::new();
        let sql = reservation::claim_sql(
            &mut p,
            key,
            &event.attempt_id,
            event.attempt_timeout_ms,
            max,
        );
        Ok(host.db.execute_with_args(&sql, &p.into_args())? > 0)
    }

    /// The slots `key` holds: what the claim enforces, for the status endpoint
    /// and the rejection message.
    fn held_count(host: &Host, key: &LimitKey) -> Result<u32, SdkError> {
        let mut p = Params::new();
        let sql = reservation::held_count_sql(&mut p, key);
        Ok(host
            .db
            .query_one_with_args::<SubmissionCount>(&sql, &p.into_args())?
            .map(|r| r.count)
            .unwrap_or(0) as u32)
    }

    /// The configured cap, `None` when the gate is off for this resource
    /// (disabled, unlimited, or unresolvable config).
    fn active_cap(host: &Host, problem_id: i32, contest_id: Option<i32>) -> Option<u32> {
        let eff = match host.config.get_effective("limits", problem_id, contest_id) {
            Ok(e) => e,
            Err(e) => {
                let _ = host.log.info(&format!(
                    "[submission-limit] Failed to resolve config: {e}, using default (unlimited)"
                ));
                return None;
            }
        };
        if !eff.is_enabled {
            return None;
        }
        let config: LimitsConfig = eff.parse_config().unwrap_or_default();
        config.max_submissions.filter(|&max| max > 0)
    }

    /// Reserve a slot for this attempt, or reject once the cap is reached.
    #[plugin_fn]
    pub fn check_limit(input: String) -> FnResult<String> {
        let host = Host::new();
        let event: BeforeSubmissionEvent = serde_json::from_str(&input)?;
        let Some(max) = active_cap(&host, event.problem_id, event.contest_id) else {
            return Ok(serde_json::to_string(&HookResponse::pass())?);
        };
        let key = LimitKey {
            user_id: event.user_id,
            problem_id: event.problem_id,
            contest_id: event.contest_id,
        };
        if try_claim(&host, &key, &event, max)? {
            return Ok(serde_json::to_string(&HookResponse::pass())?);
        }

        let count = held_count(&host, &key)?;
        let resp = HookResponse::reject(
            "SUBMISSION_LIMIT_EXCEEDED",
            format!("Submission limit reached ({}/{})", count, max),
            429,
            Some(serde_json::json!({
                "submissions_made": count,
                "max_submissions": max,
            })),
        );
        Ok(serde_json::to_string(&resp)?)
    }

    // API: GET /api/plugins/submission-limit/contests/{contest_id}/problems/{problem_id}/status
    // API: GET /api/plugins/submission-limit/problems/{problem_id}/status

    #[derive(Serialize)]
    struct LimitStatusResponse {
        /// Whether the submission-limit plugin is enabled for this resource.
        enabled: bool,
        submissions_made: u32,
        max_submissions: u32,
        remaining: Option<u32>,
        unlimited: bool,
        source: ConfigSource,
    }

    #[plugin_fn]
    pub fn get_limit_status(input: String) -> FnResult<String> {
        run_api_handler(&input, handle_limit_status)
    }

    #[plugin_fn]
    pub fn get_limit_status_standalone(input: String) -> FnResult<String> {
        run_api_handler(&input, handle_limit_status)
    }

    fn handle_limit_status(
        host: &Host,
        req: &PluginHttpRequest,
    ) -> Result<PluginHttpResponse, ApiError> {
        let user_id = req
            .require_user_id()
            .map_err(|_| PluginHttpResponse::error(401, "Authentication required"))?;

        let contest_id: Option<i32> = req.params.get("contest_id").and_then(|s| s.parse().ok());
        let problem_id: i32 = req.param("problem_id")?;

        // Contest access check (only when contest_id is present)
        if let Some(contest_id) = contest_id {
            contest::check_problem_access(host, req, contest_id, user_id, problem_id)?;
        }

        let eff = host
            .config
            .get_effective("limits", problem_id, contest_id)?;

        if !eff.is_enabled {
            return Ok(PluginHttpResponse {
                status: 200,
                headers: None,
                body: Some(serde_json::to_value(LimitStatusResponse {
                    enabled: false,
                    submissions_made: 0,
                    max_submissions: 0,
                    remaining: None,
                    unlimited: true,
                    source: eff.source,
                })?),
            });
        }

        let config: LimitsConfig = eff.parse_config().unwrap_or_default();
        let max = config.max_submissions.unwrap_or(0);
        let unlimited = max == 0;

        let count = held_count(
            host,
            &LimitKey {
                user_id,
                problem_id,
                contest_id,
            },
        )?;

        let remaining = if unlimited {
            None
        } else {
            Some(max.saturating_sub(count))
        };

        Ok(PluginHttpResponse {
            status: 200,
            headers: None,
            body: Some(serde_json::to_value(LimitStatusResponse {
                enabled: true,
                submissions_made: count,
                max_submissions: max,
                remaining,
                unlimited,
                source: eff.source,
            })?),
        })
    }
}
