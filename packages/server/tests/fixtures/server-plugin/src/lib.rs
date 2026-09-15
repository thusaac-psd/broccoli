use std::collections::HashMap;

use extism_pdk::{FnResult, host_fn, plugin_fn};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct PluginHttpRequest {
    pub method: String,
    pub params: HashMap<String, String>,
    pub query: HashMap<String, String>,
    pub body: Option<serde_json::Value>,
    #[serde(default)]
    pub auth: Option<PluginHttpAuth>,
}

#[derive(Deserialize)]
struct PluginHttpAuth {
    pub user_id: i32,
}

#[derive(Serialize)]
struct PluginHttpResponse {
    pub status: u16,
    pub body: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct HostDbResponse {
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[host_fn]
extern "ExtismHost" {
    fn store_set(input: String);
    fn store_get(input: String) -> String;
    fn db_execute(sql: String, args: String) -> String;
    fn db_query(sql: String, args: String) -> String;
}

#[plugin_fn]
pub fn reflect(input: String) -> FnResult<String> {
    let req: PluginHttpRequest = serde_json::from_str(&input)?;
    Ok(serde_json::to_string(&PluginHttpResponse {
        status: 200,
        body: Some(serde_json::json!({
            "method": req.method,
            "params": req.params,
            "query": req.query,
            "auth_user_id": req.auth.map(|auth| auth.user_id),
        })),
    })?)
}

#[plugin_fn]
pub fn kv_write(input: String) -> FnResult<String> {
    let req: PluginHttpRequest = serde_json::from_str(&input)?;
    let key = req.params.get("key").cloned().unwrap();
    let val = req.body.and_then(|b| b.get("value").cloned()).unwrap();

    let store_input = serde_json::json!({
        "entries": [{ "key": key, "value": val.as_str().unwrap() }],
    });
    unsafe {
        store_set(serde_json::to_string(&store_input)?)?;
    }
    Ok(serde_json::to_string(&PluginHttpResponse {
        status: 200,
        body: None,
    })?)
}

#[plugin_fn]
pub fn kv_read(input: String) -> FnResult<String> {
    let req: PluginHttpRequest = serde_json::from_str(&input)?;
    let key = req.params.get("key").cloned().unwrap();

    let store_input = serde_json::json!({ "keys": [key] });
    let raw = unsafe { store_get(serde_json::to_string(&store_input)?)? };

    let result: serde_json::Value = serde_json::from_str(&raw)?;
    let (status, body) = match result
        .get("values")
        .and_then(|v| v.get(&key))
        .and_then(|v| v.as_str())
    {
        Some(v) => (200, serde_json::json!({ "value": v })),
        None => (404, serde_json::json!(null)),
    };
    Ok(serde_json::to_string(&PluginHttpResponse {
        status,
        body: Some(body),
    })?)
}

// -- Visibility querier (`[[server.queries]] topic = "visibility"`) -------
//
// Hand-rolled mirrors of `broccoli-types`' `VisibilityQueryInput` /
// `VisibilityQueryOutput` / `WireDecision` wire shapes, matching the style
// already used above for the HTTP request/response types rather than
// pulling in `broccoli-server-sdk` as a new dependency of this fixture.

#[derive(Deserialize)]
struct VisibilityQuerySubjectIn {
    #[allow(dead_code)]
    user_id: Option<i32>,
    #[allow(dead_code)]
    authenticated: bool,
    #[allow(dead_code)]
    #[serde(default)]
    permissions: Vec<String>,
}

#[derive(Deserialize)]
struct VisibilityQueryContextIn {
    #[allow(dead_code)]
    contest_id: Option<i32>,
}

#[derive(Deserialize)]
struct VisibilityQueryResourceIn {
    kind: String,
    id: String,
    #[allow(dead_code)]
    #[serde(default)]
    contest_id: Option<i32>,
    #[allow(dead_code)]
    #[serde(default)]
    problem_id: Option<i32>,
}

#[derive(Deserialize)]
struct VisibilityQueryInputIn {
    #[allow(dead_code)]
    subject: VisibilityQuerySubjectIn,
    #[allow(dead_code)]
    action: String,
    #[allow(dead_code)]
    context: VisibilityQueryContextIn,
    resources: Vec<VisibilityQueryResourceIn>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum WireDecisionOut {
    Allow {},
    #[allow(dead_code)]
    Deny {},
    Redact { fields: Vec<String> },
}

#[derive(Serialize)]
struct VisibilityQueryOutputOut {
    decisions: Vec<WireDecisionOut>,
}

/// Test-only visibility querier. Deliberately tries two things a plugin must
/// never be able to pull off, so the integration suite
/// (`tests/integration/visibility_plugin.rs`) can prove the host refuses
/// both:
///
/// - It answers `Allow` for EVERY resource by default, including ones the
///   host has already denied. `Decision::meet` must keep the host's `Deny`
///   no matter what a plugin answers - `plugin_cannot_widen_host_decision`.
/// - For submission ids nominated via the `redact_submission_ids` KV key
///   (seeded through the existing `kv_write` route/host functions above),
///   it answers `Redact` on `result.verdict` / `result.score`.
///   `WireDecision::Redact` only ever carries field PATHS, never a
///   replacement value, so blanking those two fields is the closest a
///   plugin can get to "authoring" a verdict - the masked fields must come
///   back `null`, never plugin-supplied content -
///   `plugin_cannot_author_a_verdict`.
#[plugin_fn]
pub fn decide_visibility(input: String) -> FnResult<String> {
    let req: VisibilityQueryInputIn = serde_json::from_str(&input)?;
    let redact_ids = read_kv_csv("redact_submission_ids");

    let decisions = req
        .resources
        .iter()
        .map(|r| {
            if r.kind == "submission" && redact_ids.iter().any(|id| id == &r.id) {
                WireDecisionOut::Redact {
                    fields: vec!["result.verdict".to_string(), "result.score".to_string()],
                }
            } else {
                WireDecisionOut::Allow {}
            }
        })
        .collect();

    Ok(serde_json::to_string(&VisibilityQueryOutputOut { decisions })?)
}

/// Read a comma-separated KV value written via the `kv_write` route. Empty
/// (never written, host error, or non-JSON) reads back as no ids - this
/// query function must never trap or fail the batch just because a test
/// hasn't seeded the key yet.
fn read_kv_csv(key: &str) -> Vec<String> {
    let Ok(store_input) = serde_json::to_string(&serde_json::json!({ "keys": [key] })) else {
        return Vec::new();
    };
    let Ok(raw) = (unsafe { store_get(store_input) }) else {
        return Vec::new();
    };
    let Ok(result) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    result
        .get("values")
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_str())
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[plugin_fn]
pub fn sql_parameterized(input: String) -> FnResult<String> {
    let req: PluginHttpRequest = serde_json::from_str(&input)?;
    let body = req.body.unwrap_or_default();
    let name = body["name"].as_str().unwrap_or("unknown");

    unsafe {
        db_execute(
            "CREATE TABLE IF NOT EXISTS p_names (name TEXT)".into(),
            "[]".into(),
        )?;
    }

    let args = serde_json::to_string(&vec![name])?;
    unsafe {
        db_execute("INSERT INTO p_names (name) VALUES ($1)".into(), args)?;
    }

    let query_args = serde_json::to_string(&vec![name])?;
    let res_json = unsafe {
        db_query(
            "SELECT name FROM p_names WHERE name = $1".into(),
            query_args,
        )?
    };
    let res: HostDbResponse = serde_json::from_str(&res_json)?;

    let rows: Vec<serde_json::Value> = serde_json::from_value(res.data.unwrap_or_default())?;

    Ok(serde_json::to_string(&PluginHttpResponse {
        status: 200,
        body: Some(serde_json::json!({ "found": rows.len() })),
    })?)
}
