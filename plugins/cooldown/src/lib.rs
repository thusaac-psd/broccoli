pub mod gate;

#[cfg(target_arch = "wasm32")]
mod plugin {
    use broccoli_server_sdk::error::SdkError;
    use broccoli_server_sdk::prelude::*;
    use broccoli_server_sdk::types::ConfigSource;
    use extism_pdk::{FnResult, plugin_fn};
    use serde::{Deserialize, Serialize};

    use crate::gate::{self, Clocks, CooldownKey};

    #[derive(Debug, Default, Deserialize)]
    struct CooldownConfig {
        #[serde(default)]
        cooldown_seconds: Option<u32>,
    }

    /// Schema bootstrap, invoked by the host on plugin load.
    #[plugin_fn]
    pub fn init() -> FnResult<String> {
        let host = Host::new();
        host.db.ensure_schema("cooldown", &[gate::SCHEMA_SQL])?;
        Ok("ok".into())
    }

    fn clocks(host: &Host, key: &CooldownKey) -> Result<Clocks, SdkError> {
        let mut p = Params::new();
        let sql = gate::clocks_sql(&mut p, key);
        Ok(host
            .db
            .query_one_with_args::<Clocks>(&sql, &p.into_args())?
            .unwrap_or_default())
    }

    /// The configured cooldown, `None` when the gate is off for this resource
    /// (disabled, zero, or unresolvable config).
    fn active_cooldown(host: &Host, problem_id: i32, contest_id: Option<i32>) -> Option<u32> {
        let eff = match host
            .config
            .get_effective("cooldown", problem_id, contest_id)
        {
            Ok(e) => e,
            Err(e) => {
                let _ = host.log.info(&format!(
                    "[cooldown] Failed to resolve config: {e}, using default (disabled)"
                ));
                return None;
            }
        };
        if !eff.is_enabled {
            return None;
        }
        let config: CooldownConfig = eff.parse_config().unwrap_or_default();
        config.cooldown_seconds.filter(|&c| c > 0)
    }

    fn reject_response(remaining: u64, cooldown: u32) -> FnResult<String> {
        let resp = HookResponse::reject(
            "COOLDOWN_ACTIVE",
            format!(
                "Please wait {} more second{}",
                remaining,
                if remaining == 1 { "" } else { "s" }
            ),
            429,
            Some(serde_json::json!({
                "remaining_seconds": remaining,
                "cooldown_seconds": cooldown,
            })),
        );
        Ok(serde_json::to_string(&resp)?)
    }

    /// Pass and claim the slot when neither clock blocks, else reject with the
    /// time left. Passing costs one statement.
    #[plugin_fn]
    pub fn check_cooldown(input: String) -> FnResult<String> {
        let host = Host::new();
        let event: BeforeSubmissionEvent = serde_json::from_str(&input)?;
        let Some(cooldown) = active_cooldown(&host, event.problem_id, event.contest_id) else {
            return Ok(serde_json::to_string(&HookResponse::pass())?);
        };
        let key = CooldownKey {
            user_id: event.user_id,
            problem_id: event.problem_id,
            contest_id: event.contest_id,
        };
        let mut p = Params::new();
        let sql = gate::claim_sql(&mut p, &key, cooldown, event.attempt_timeout_ms);
        if host.db.execute_with_args(&sql, &p.into_args())? > 0 {
            return Ok(serde_json::to_string(&HookResponse::pass())?);
        }
        // The clocks moved on since the claim lost; never report 0 for a rejection.
        let wait = clocks(&host, &key)?.wait_secs(cooldown).max(1);
        reject_response(wait, cooldown)
    }

    #[derive(Serialize)]
    struct CooldownStatusResponse {
        /// Whether the cooldown plugin is enabled for this resource.
        enabled: bool,
        cooldown_seconds: u32,
        seconds_since_last: Option<i64>,
        can_submit: bool,
        source: ConfigSource,
    }

    // API: GET /api/plugins/cooldown/contests/{contest_id}/problems/{problem_id}/status
    #[plugin_fn]
    pub fn get_cooldown_status(input: String) -> FnResult<String> {
        run_api_handler(&input, handle_cooldown_status)
    }

    // API: GET /api/plugins/cooldown/problems/{problem_id}/status
    #[plugin_fn]
    pub fn get_cooldown_status_standalone(input: String) -> FnResult<String> {
        run_api_handler(&input, handle_cooldown_status)
    }

    fn handle_cooldown_status(
        host: &Host,
        req: &PluginHttpRequest,
    ) -> Result<PluginHttpResponse, ApiError> {
        // The host checked the caller may reach the problem (as a
        // participant, inside a contest) before this runs.
        let user_id = req.require_user_id()?;
        let contest_id = req.contest.as_ref().map(|c| c.id);
        let problem_id = req.problem_id()?;

        let eff = host
            .config
            .get_effective("cooldown", problem_id, contest_id)?;

        if !eff.is_enabled {
            return Ok(PluginHttpResponse {
                status: 200,
                headers: None,
                body: Some(serde_json::to_value(CooldownStatusResponse {
                    enabled: false,
                    cooldown_seconds: 0,
                    seconds_since_last: None,
                    can_submit: true,
                    source: eff.source,
                })?),
            });
        }

        let config: CooldownConfig = eff.parse_config().unwrap_or_default();
        let cooldown = config.cooldown_seconds.unwrap_or(0);
        let clocks = clocks(
            host,
            &CooldownKey {
                user_id,
                problem_id,
                contest_id,
            },
        )?;
        let seconds_since_last = clocks.since_submission.map(|s| s.max(0.0) as i64);
        let can_submit = clocks.wait_secs(cooldown) == 0;

        Ok(PluginHttpResponse {
            status: 200,
            headers: None,
            body: Some(serde_json::to_value(CooldownStatusResponse {
                enabled: true,
                cooldown_seconds: cooldown,
                seconds_since_last,
                can_submit,
                source: eff.source,
            })?),
        })
    }
}
