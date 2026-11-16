//! Station endpoint client, one per upstream, authed by a PrintStation header.

use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;
use ureq::Agent;

use crate::config::ServerCfg;

const BASE_PATH: &str = "/api/v1/p/print/api/plugins/print";

/// A claimable job, with the fields the station renders and routes by.
#[derive(Debug, Clone, Deserialize)]
pub struct Job {
    pub id: i64,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub problem_label: Option<String>,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub target_printer: Option<String>,
    #[serde(default)]
    pub created_at: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct JobsResponse {
    #[serde(default)]
    data: Vec<Job>,
}

#[derive(Debug, Deserialize)]
struct HeartbeatResponse {
    station: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed,
    Taken,
}

#[derive(Clone)]
pub struct ServerClient {
    agent: Agent,
    base: String,
    auth: String,
    /// The contest whose station list holds the token, sent with every
    /// request so the server checks the token against that list.
    contest: Option<String>,
    pub label: String,
}

impl ServerClient {
    pub fn new(server: &ServerCfg) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build();
        let base = format!("{}{BASE_PATH}", server.url.trim_end_matches('/'));
        Self {
            agent,
            base,
            auth: format!("PrintStation {}", server.token),
            contest: server.contest.map(|id| id.to_string()),
            label: server.url.clone(),
        }
    }

    /// A request to the station route at `path`, carrying this station's
    /// credential.
    fn request(&self, method: &str, path: &str) -> ureq::Request {
        let req = self
            .agent
            .request(method, &format!("{}{path}", self.base))
            .set("Authorization", &self.auth);
        match &self.contest {
            Some(id) => req.query("contest_id", id),
            None => req,
        }
    }

    /// The server names this station after the credential in `auth`, and
    /// answers with that name.
    pub fn heartbeat(&self, printers: &[String], queue_seen: i64) -> Result<String> {
        let body = json!({
            "printers": printers,
            "version": env!("CARGO_PKG_VERSION"),
            "queue_seen": queue_seen,
        });
        let resp: HeartbeatResponse = self
            .request("POST", "/stations/heartbeat")
            .send_json(body)
            .map_err(status_error)?
            .into_json()?;
        Ok(resp.station)
    }

    pub fn fetch_jobs(&self, limit: usize) -> Result<Vec<Job>> {
        let resp = self
            .request("GET", "/stations/jobs")
            .query("limit", &limit.to_string())
            .call()
            .map_err(status_error)?;
        let parsed: JobsResponse = resp.into_json()?;
        Ok(parsed.data)
    }

    /// A 409 means another station won the claim.
    pub fn claim(&self, id: i64, printer: &str) -> Result<ClaimOutcome> {
        let body = json!({ "printer": printer });
        match self
            .request("POST", &format!("/stations/jobs/{id}/claim"))
            .send_json(body)
        {
            Ok(_) => Ok(ClaimOutcome::Claimed),
            Err(ureq::Error::Status(409, _)) => Ok(ClaimOutcome::Taken),
            Err(e) => Err(status_error(e)),
        }
    }

    pub fn report(
        &self,
        id: i64,
        status: &str,
        pages: Option<u32>,
        error: Option<&str>,
    ) -> Result<()> {
        let body = json!({ "status": status, "pages": pages, "error": error });
        self.request("POST", &format!("/stations/jobs/{id}/status"))
            .send_json(body)
            .map_err(status_error)?;
        Ok(())
    }
}

/// Surface the server's error message when there is one.
fn status_error(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, resp) => {
            let msg = resp
                .into_json::<serde_json::Value>()
                .ok()
                .and_then(|v| v.get("message")?.as_str().map(String::from))
                .unwrap_or_else(|| format!("HTTP {code}"));
            anyhow!("server returned {code}: {msg}")
        }
        ureq::Error::Transport(t) => anyhow!("connection failed: {t}"),
    }
}
