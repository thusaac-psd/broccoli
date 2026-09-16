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
    Deny {},
    Redact { fields: Vec<String> },
}

#[derive(Serialize)]
struct VisibilityQueryOutputOut {
    decisions: Vec<WireDecisionOut>,
}

/// Test-only visibility querier, switched between behaviors by the
/// `visibility_mode` KV key (seeded through the existing `kv_write`
/// route/host functions above). Deliberately tries several things a plugin
/// must never be able to pull off, so the integration suite
/// (`tests/integration/visibility_plugin.rs`) can prove the host refuses all
/// of them without ever surfacing a 500:
///
/// - unset/`"normal"` (the default): a WORKING decision function. It answers
///   `Allow` for EVERY resource by default, including ones the host has
///   already denied - `Decision::meet` must keep the host's `Deny` no matter
///   what a plugin answers (`plugin_cannot_widen_host_decision`). Resources
///   nominated (by `"kind:id"`) via `deny_resource_keys` get `Deny`;
///   resources nominated via `redact_resource_keys` get `Redact` with the
///   fields from `redact_resource_fields` (defaulting to `label` /
///   `problem_title`, the contest-problem list DTO's own fields); submission
///   ids nominated via the older, submission-only `redact_submission_ids`
///   key get `Redact` on `result.verdict` / `result.score` - kept unchanged
///   so `plugin_cannot_author_a_verdict` still exercises exactly the
///   mechanism it always has. `WireDecision::Redact` only ever carries field
///   PATHS, never a replacement value, so blanking fields is the closest a
///   plugin can get to "authoring" content - the masked fields must come
///   back `null`/`[]`, never plugin-supplied content.
/// - `"trap"`: panics before even parsing `input`, forcing a genuine WASM
///   trap (this target has no unwind support, so a panic lowers to
///   `unreachable`).
/// - `"non_json"`: returns a successful `FnResult` whose payload is not JSON
///   at all.
/// - `"short_vector"`: returns one fewer decision than there are resources.
/// - `"unknown_variant"`: hand-crafts raw JSON using a decision tag the host
///   has never heard of, bypassing `WireDecisionOut` entirely.
/// - `"over_limit_segments"` / `"over_limit_bytes"` / `"over_limit_fields"`:
///   answers `Redact` with a field mask that exceeds one of the host's
///   `MAX_MASK_PATH_SEGMENTS` / `MAX_MASK_PATH_BYTES` / `MAX_MASK_FIELDS`
///   caps (`packages/server/src/visibility/plugin_query.rs`) respectively.
///
/// Every failure mode above must deny the WHOLE batch - never a partial
/// result, never a 500.
#[plugin_fn]
pub fn decide_visibility(input: String) -> FnResult<String> {
    let mode = read_kv_single("visibility_mode").unwrap_or_default();

    if mode == "trap" {
        panic!("decide_visibility: forced trap for failure-injection test");
    }

    let req: VisibilityQueryInputIn = serde_json::from_str(&input)?;

    if mode == "non_json" {
        return Ok("this is deliberately not JSON".to_string());
    }

    if mode == "unknown_variant" {
        // Bypass `WireDecisionOut` entirely: a tag the host has never heard
        // of, once per resource in the batch.
        let decisions: Vec<serde_json::Value> = req
            .resources
            .iter()
            .map(|_| serde_json::json!({ "mystery": {} }))
            .collect();
        return Ok(serde_json::json!({ "decisions": decisions }).to_string());
    }

    if mode == "short_vector" {
        let mut decisions: Vec<WireDecisionOut> = req
            .resources
            .iter()
            .map(|_| WireDecisionOut::Allow {})
            .collect();
        decisions.pop();
        return Ok(serde_json::to_string(&VisibilityQueryOutputOut { decisions })?);
    }

    if mode == "over_limit_segments" || mode == "over_limit_bytes" || mode == "over_limit_fields" {
        let fields = match mode.as_str() {
            // 40 dot-separated segments: over the host's 32-segment cap.
            "over_limit_segments" => vec![vec!["a"; 40].join(".")],
            // A single 300-byte segment: over the host's 256-byte cap.
            "over_limit_bytes" => vec!["x".repeat(300)],
            // 70 distinct field paths: over the host's 64-field cap.
            "over_limit_fields" => (0..70).map(|i| format!("field_{i}")).collect(),
            _ => unreachable!(),
        };
        let decisions = req
            .resources
            .iter()
            .map(|_| WireDecisionOut::Redact {
                fields: fields.clone(),
            })
            .collect();
        return Ok(serde_json::to_string(&VisibilityQueryOutputOut { decisions })?);
    }

    // -- Normal path -------------------------------------------------------
    let redact_submission_ids = read_kv_csv("redact_submission_ids");
    let deny_keys = read_kv_csv("deny_resource_keys");
    let redact_keys = read_kv_csv("redact_resource_keys");
    let redact_fields = {
        let fields = read_kv_csv("redact_resource_fields");
        if fields.is_empty() {
            vec!["label".to_string(), "problem_title".to_string()]
        } else {
            fields
        }
    };

    let decisions = req
        .resources
        .iter()
        .map(|r| {
            let key = format!("{}:{}", r.kind, r.id);
            if deny_keys.iter().any(|k| k == &key) {
                WireDecisionOut::Deny {}
            } else if redact_keys.iter().any(|k| k == &key) {
                WireDecisionOut::Redact {
                    fields: redact_fields.clone(),
                }
            } else if r.kind == "submission" && redact_submission_ids.iter().any(|id| id == &r.id)
            {
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

/// Read a single KV value written via the `kv_write` route. `None` if never
/// written, on any host error, or on a non-JSON/unexpected shape - this
/// query function must never trap or fail the batch just because a test
/// hasn't seeded the key yet.
fn read_kv_single(key: &str) -> Option<String> {
    let store_input = serde_json::to_string(&serde_json::json!({ "keys": [key] })).ok()?;
    let raw = (unsafe { store_get(store_input) }).ok()?;
    let result: serde_json::Value = serde_json::from_str(&raw).ok()?;
    result
        .get("values")
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Read a comma-separated KV value written via the `kv_write` route. Empty
/// (never written, host error, or non-JSON) reads back as no ids - this
/// query function must never trap or fail the batch just because a test
/// hasn't seeded the key yet.
fn read_kv_csv(key: &str) -> Vec<String> {
    let Some(raw) = read_kv_single(key) else {
        return Vec::new();
    };
    raw.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
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
