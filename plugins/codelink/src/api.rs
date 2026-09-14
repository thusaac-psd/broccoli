use broccoli_server_sdk::prelude::*;
use serde::Deserialize;

use crate::config::ContestConfig;
use crate::standings::{Participant, Problem, Submission, calculate};

pub fn handle_contest_info(
    host: &Host,
    req: &PluginHttpRequest,
) -> Result<PluginHttpResponse, ApiError> {
    let (contest_id, info, config) = load_context(host, req)?;
    #[derive(Deserialize)]
    struct ProblemCount {
        problem_count: usize,
    }
    let mut p = Params::new();
    let sql = format!(
        "SELECT COUNT(*) AS problem_count FROM contest_problem WHERE contest_id = {}",
        p.bind(contest_id)
    );
    let problem_count = host
        .db
        .query_one_with_args::<ProblemCount>(&sql, &p.into_args())?
        .ok_or_else(|| SdkError::Other("Missing Codelink problem count".into()))?
        .problem_count;
    let mut body = serde_json::to_value(config)?;
    body["phase"] = serde_json::json!(info.phase);
    body["problem_count"] = serde_json::json!(problem_count);
    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(body),
    })
}

fn load_context(
    host: &Host,
    req: &PluginHttpRequest,
) -> Result<(i32, contest::ContestInfo, ContestConfig), ApiError> {
    let contest_id: i32 = req
        .param("contest_id")
        .map_err(|_| PluginHttpResponse::error(400, "Invalid contest id"))?;
    let info = contest::check_access(host, req, contest_id)?;
    info.require_type("codelink")?;
    let config = ContestConfig::load(host, contest_id)?;
    Ok((contest_id, info, config))
}

pub fn handle_standings(
    host: &Host,
    req: &PluginHttpRequest,
) -> Result<PluginHttpResponse, ApiError> {
    let (contest_id, info, config) = load_context(host, req)?;

    let mut p = Params::new();
    let sql = format!(
        "SELECT problem_id, label FROM contest_problem \
         WHERE contest_id = {} ORDER BY position, problem_id",
        p.bind(contest_id)
    );
    let problems: Vec<Problem> = host.db.query_with_args(&sql, &p.into_args())?;

    let mut p = Params::new();
    let sql = format!(
        "SELECT cu.user_id, u.username FROM contest_user cu \
         JOIN \"user\" u ON u.id = cu.user_id \
         WHERE cu.contest_id = {} ORDER BY cu.user_id",
        p.bind(contest_id)
    );
    let participants: Vec<Participant> = host.db.query_with_args(&sql, &p.into_args())?;

    let mut p = Params::new();
    let sql = format!(
        "SELECT s.id AS submission_id, s.user_id, s.problem_id, \
             (EXTRACT(EPOCH FROM (s.created_at - c.start_time)) * 1000000)::bigint \
                 AS submitted_at_us, \
             (j.is_finalized AND j.status = 'Judged' AND j.verdict = 'Accepted') \
                 IS TRUE AS accepted, NOT j.is_finalized AS pending \
         FROM submission s \
         JOIN contest c ON c.id = s.contest_id \
         JOIN submission_judgement j ON j.submission_id = s.id \
         JOIN contest_user cu ON cu.contest_id = c.id AND cu.user_id = s.user_id \
         JOIN contest_problem cp ON cp.contest_id = c.id AND cp.problem_id = s.problem_id \
         WHERE s.contest_id = {} AND s.contest_type = 'codelink' \
             AND s.created_at >= c.start_time AND s.created_at <= c.end_time \
             AND j.is_current = TRUE \
         ORDER BY s.created_at, s.id",
        p.bind(contest_id)
    );
    let submissions: Vec<Submission> = host.db.query_with_args(&sql, &p.into_args())?;
    let standings = calculate(participants, problems, submissions, &config)?;
    let mut body = serde_json::to_value(standings)?;
    body["phase"] = serde_json::json!(info.phase);

    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(body),
    })
}
