//! `print_job` persistence. Values bind through `Params`; writes use the host
//! execute path since the query path wraps SQL in json_agg.

use broccoli_server_sdk::Host;
use broccoli_server_sdk::error::SdkError;
use broccoli_server_sdk::prelude::Params;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::lifecycle::{self, Change, JobStatus, bind_opt};
use crate::models::{JobRow, StationJob};

// Timestamps come back as epoch seconds.
const JOB_COLS: &str = "id, contest_id, user_id, username, display_name, problem_label, \
    submission_id, language, filename, pages_est, pages, location, target_printer, status, \
    claimed_by, claimed_printer, error, \
    EXTRACT(EPOCH FROM created_at) AS created_at, \
    EXTRACT(EPOCH FROM claimed_at) AS claimed_at, \
    EXTRACT(EPOCH FROM printed_at) AS printed_at";

const STATION_JOB_COLS: &str = "id, contest_id, username, display_name, problem_label, \
    language, filename, source, location, target_printer, \
    EXTRACT(EPOCH FROM created_at) AS created_at";

#[derive(Debug, Clone)]
pub struct NewJob {
    pub contest_id: Option<i32>,
    pub user_id: i32,
    pub username: String,
    pub display_name: Option<String>,
    pub problem_label: Option<String>,
    pub submission_id: Option<i32>,
    pub language: String,
    pub filename: String,
    pub source: String,
    pub pages_est: i32,
    pub status: JobStatus,
}

#[derive(Debug, Clone)]
pub struct JobFilters {
    pub status: Option<JobStatus>,
    pub contest_id: Option<i32>,
    pub station: Option<String>,
    pub printer: Option<String>,
    pub search: Option<String>,
    pub page: i64,
    pub per_page: i64,
    pub sort_by: String,
    pub sort_order: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobWithSource {
    pub id: i64,
    pub source: String,
    pub language: String,
    pub filename: String,
    pub username: String,
    pub display_name: Option<String>,
}

pub fn fetch_job_source(host: &Host, id: i64) -> Result<Option<JobWithSource>, SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "SELECT id, source, language, filename, username, display_name FROM print_job WHERE id = {}",
        p.bind(id)
    );
    host.db.query_one_with_args(&sql, &p.into_args())
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubmissionRow {
    pub files: Value,
    pub language: String,
    pub user_id: i32,
    pub username: String,
    pub contest_id: Option<i32>,
    pub problem_id: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProblemLabelRow {
    pub label: String,
    pub position: i32,
}

#[derive(Deserialize)]
struct CountRow {
    count: i64,
}

const INSERT_COLS: &str = "contest_id, user_id, username, display_name, problem_label, \
    submission_id, language, filename, source, pages_est, status";

/// Bind one job's values into `p` and return its `(...)` VALUES tuple. Binding
/// left-to-right keeps placeholder numbering in column order, and threading a
/// single shared `Params` lets a multi-row INSERT number rows monotonically
/// ($1..$11, $12..$22, ...).
fn bind_row(p: &mut Params, job: &NewJob) -> String {
    format!(
        "({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {})",
        bind_opt(p, job.contest_id),
        p.bind(job.user_id),
        p.bind(job.username.clone()),
        bind_opt(p, job.display_name.clone()),
        bind_opt(p, job.problem_label.clone()),
        bind_opt(p, job.submission_id),
        p.bind(job.language.clone()),
        p.bind(job.filename.clone()),
        p.bind(job.source.clone()),
        p.bind(job.pages_est),
        p.bind(job.status.as_str()),
    )
}

fn build_insert(job: &NewJob) -> (String, Vec<Value>) {
    let mut p = Params::new();
    let sql = format!(
        "INSERT INTO print_job ({INSERT_COLS}) VALUES {}",
        bind_row(&mut p, job)
    );
    (sql, p.into_args())
}

pub fn insert_job(host: &Host, job: &NewJob) -> Result<u64, SdkError> {
    let (sql, args) = build_insert(job);
    host.db.execute_with_args(&sql, &args)
}

/// Insert every job or none, so a partial submission never reaches the queue.
///
/// This is a SINGLE multi-row INSERT, deliberately NOT a `begin()`/`commit()`
/// transaction over per-row inserts. A single statement is already atomic in
/// Postgres -- every row commits or the whole statement aborts on the first
/// constraint violation -- so it gives the all-or-nothing guarantee without a
/// plugin-side transaction. That matters here: an open plugin transaction pins
/// one of the tiny plugin DB pool's connections for the life of the guest call
/// (the host bridges each guest DB op with `block_on`), so a burst of
/// multi-file prints -- each holding a txn across several round-trips -- can
/// exhaust the pool and deadlock the runtime. See the plugin "no transactions
/// under concurrency" rule.
pub fn insert_jobs(host: &Host, jobs: &[NewJob]) -> Result<u64, SdkError> {
    if jobs.is_empty() {
        return Ok(0);
    }
    let mut p = Params::new();
    let rows: Vec<String> = jobs.iter().map(|job| bind_row(&mut p, job)).collect();
    let sql = format!(
        "INSERT INTO print_job ({INSERT_COLS}) VALUES {}",
        rows.join(", ")
    );
    host.db.execute_with_args(&sql, &p.into_args())
}

/// Take `jobs` of `user_id`'s quota of `max` jobs in `contest_id`. False when
/// that would exceed the quota. One statement: concurrent claims serialize on
/// the quota row and the `WHERE` re-checks the count a concurrent winner
/// committed, so two prints can never both take the last job.
pub fn claim_quota(
    host: &Host,
    contest_id: i32,
    user_id: i32,
    jobs: i32,
    max: i32,
) -> Result<bool, SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "INSERT INTO print_quota (contest_id, user_id, jobs) \
         SELECT {contest}::int, {user}::int, {jobs}::int WHERE {jobs}::int <= {max}::int \
         ON CONFLICT (contest_id, user_id) DO UPDATE SET jobs = print_quota.jobs + EXCLUDED.jobs \
         WHERE print_quota.jobs + EXCLUDED.jobs <= {max}::int",
        contest = p.bind(contest_id),
        user = p.bind(user_id),
        jobs = p.bind(jobs),
        max = p.bind(max),
    );
    Ok(host.db.execute_with_args(&sql, &p.into_args())? > 0)
}

/// Return `jobs` claimed by [`claim_quota`] for prints that were never queued.
pub fn refund_quota(host: &Host, contest_id: i32, user_id: i32, jobs: i32) -> Result<(), SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "UPDATE print_quota SET jobs = GREATEST(jobs - {}::int, 0) \
         WHERE contest_id = {} AND user_id = {}",
        p.bind(jobs),
        p.bind(contest_id),
        p.bind(user_id)
    );
    host.db.execute_with_args(&sql, &p.into_args())?;
    Ok(())
}

pub fn fetch_submission(
    host: &Host,
    submission_id: i32,
) -> Result<Option<SubmissionRow>, SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "SELECT s.files, s.language, s.user_id, u.username, s.contest_id, s.problem_id \
         FROM submission s JOIN \"user\" u ON u.id = s.user_id WHERE s.id = {}",
        p.bind(submission_id)
    );
    host.db.query_one_with_args(&sql, &p.into_args())
}

pub fn fetch_problem_label(
    host: &Host,
    contest_id: i32,
    problem_id: i32,
) -> Result<Option<ProblemLabelRow>, SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "SELECT label, position FROM contest_problem WHERE contest_id = {} AND problem_id = {}",
        p.bind(contest_id),
        p.bind(problem_id)
    );
    host.db.query_one_with_args(&sql, &p.into_args())
}

pub fn list_my_jobs(
    host: &Host,
    user_id: i32,
    contest_id: Option<i32>,
) -> Result<Vec<JobRow>, SdkError> {
    let mut p = Params::new();
    let mut sql = format!(
        "SELECT {JOB_COLS} FROM print_job WHERE user_id = {}",
        p.bind(user_id)
    );
    if let Some(cid) = contest_id {
        sql.push_str(&format!(" AND contest_id = {}", p.bind(cid)));
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT 200");
    host.db.query_with_args(&sql, &p.into_args())
}

fn build_filter_where(p: &mut Params, f: &JobFilters) -> String {
    let mut clauses: Vec<String> = Vec::new();
    if let Some(s) = f.status {
        clauses.push(format!("status = {}", p.bind(s.as_str())));
    }
    if let Some(cid) = f.contest_id {
        clauses.push(format!("contest_id = {}", p.bind(cid)));
    }
    if let Some(station) = f.station.as_ref().filter(|s| !s.is_empty()) {
        clauses.push(format!("claimed_by = {}", p.bind(station.clone())));
    }
    if let Some(printer) = f.printer.as_ref().filter(|s| !s.is_empty()) {
        clauses.push(format!("claimed_printer = {}", p.bind(printer.clone())));
    }
    if let Some(term) = f
        .search
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        let ph = p.bind(format!("%{term}%"));
        clauses.push(format!(
            "(username ILIKE {ph} OR COALESCE(display_name, '') ILIKE {ph} \
             OR filename ILIKE {ph} OR COALESCE(problem_label, '') ILIKE {ph})"
        ));
    }
    if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    }
}

fn sort_column(sort_by: &str) -> &'static str {
    match sort_by {
        "id" => "id",
        "status" => "status",
        "contest_id" => "contest_id",
        "username" => "username",
        _ => "created_at",
    }
}

fn sort_direction(order: &str) -> &'static str {
    if order.eq_ignore_ascii_case("asc") {
        "ASC"
    } else {
        "DESC"
    }
}

/// Returns the page of rows plus the total count.
pub fn list_admin_jobs(host: &Host, f: &JobFilters) -> Result<(Vec<JobRow>, i64), SdkError> {
    let mut pc = Params::new();
    let where_clause = build_filter_where(&mut pc, f);
    let count_sql = format!("SELECT COUNT(*) AS count FROM print_job {where_clause}");
    let total = host
        .db
        .query_one_with_args::<CountRow>(&count_sql, &pc.into_args())?
        .map(|r| r.count)
        .unwrap_or(0);

    let mut pp = Params::new();
    let where_clause = build_filter_where(&mut pp, f);
    let per_page = f.per_page.clamp(1, 200);
    let page = f.page.max(1);
    let offset = (page - 1) * per_page;
    let limit_ph = pp.bind(per_page);
    let offset_ph = pp.bind(offset);
    let page_sql = format!(
        "SELECT {JOB_COLS} FROM print_job {where_clause} ORDER BY {} {} LIMIT {limit_ph} OFFSET {offset_ph}",
        sort_column(&f.sort_by),
        sort_direction(&f.sort_order),
    );
    let rows = host
        .db
        .query_with_args::<JobRow>(&page_sql, &pp.into_args())?;
    Ok((rows, total))
}

/// Jobs a station may claim now, oldest first.
pub fn list_station_jobs(
    host: &Host,
    contest_filter: Option<i32>,
    location: Option<&str>,
    limit: i64,
) -> Result<Vec<StationJob>, SdkError> {
    let mut p = Params::new();
    let mut sql = format!(
        "SELECT {STATION_JOB_COLS} FROM print_job WHERE {}",
        lifecycle::claimable(&mut p)
    );
    if let Some(cid) = contest_filter {
        sql.push_str(&format!(" AND contest_id = {}", p.bind(cid)));
    }
    if let Some(loc) = location.filter(|l| !l.is_empty()) {
        sql.push_str(&format!(
            " AND (location IS NULL OR location = {})",
            p.bind(loc.to_string())
        ));
    }
    sql.push_str(&format!(
        " ORDER BY created_at ASC LIMIT {}",
        p.bind(limit.clamp(1, 100))
    ));
    host.db.query_with_args(&sql, &p.into_args())
}

/// Perform `change` on job `id`. Returns false when the job is not in a state
/// the move may leave (or is outside `contest_filter`); concurrent movers
/// serialize on the row, so exactly one of them wins.
pub fn apply(
    host: &Host,
    id: i64,
    change: &Change<'_>,
    contest_filter: Option<i32>,
) -> Result<bool, SdkError> {
    let mut p = Params::new();
    let sql = lifecycle::transition_sql(&mut p, id, change, contest_filter);
    Ok(host.db.execute_with_args(&sql, &p.into_args())? == 1)
}

#[derive(Deserialize)]
struct StatusRow {
    status: JobStatus,
}

/// The job's current state, `None` when it does not exist (in `contest_filter`).
pub fn job_status(
    host: &Host,
    id: i64,
    contest_filter: Option<i32>,
) -> Result<Option<JobStatus>, SdkError> {
    let mut p = Params::new();
    let mut sql = format!("SELECT status FROM print_job WHERE id = {}", p.bind(id));
    if let Some(cid) = contest_filter {
        sql.push_str(&format!(" AND contest_id = {}", p.bind(cid)));
    }
    Ok(host
        .db
        .query_one_with_args::<StatusRow>(&sql, &p.into_args())?
        .map(|r| r.status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn new_job() -> NewJob {
        NewJob {
            contest_id: Some(1),
            user_id: 10,
            username: "alice".into(),
            display_name: None,
            problem_label: Some("A".into()),
            submission_id: Some(42),
            language: "cpp".into(),
            filename: "main.cpp".into(),
            source: "int main(){}".into(),
            pages_est: 1,
            status: JobStatus::Pending,
        }
    }

    #[test]
    fn insert_uses_print_job_table() {
        let host = Host::mock();
        host.db.queue_execute_result(1);
        let affected = insert_job(&host, &new_job()).unwrap();
        assert_eq!(affected, 1);
        let exec = &host.db.executions()[0].sql;
        assert!(exec.contains("INSERT INTO print_job"));
        assert!(exec.contains("$1"));
    }

    #[test]
    fn insert_jobs_empty_is_noop() {
        let host = Host::mock();
        assert_eq!(insert_jobs(&host, &[]).unwrap(), 0);
    }

    #[test]
    fn insert_jobs_emits_one_multi_row_statement() {
        // The all-or-nothing guarantee must come from a SINGLE multi-row INSERT,
        // never a begin()/commit() over per-row inserts: a plugin-side transaction
        // pins a plugin-pool connection and deadlocks the runtime under concurrent
        // multi-file prints. Assert exactly one execution carrying both rows.
        let host = Host::mock();
        host.db.queue_execute_result(2);
        let affected = insert_jobs(&host, &[new_job(), new_job()]).unwrap();
        assert_eq!(affected, 2);

        let execs = host.db.executions();
        assert_eq!(
            execs.len(),
            1,
            "insert_jobs must issue ONE statement, not a per-row transaction loop"
        );
        let sql = &execs[0].sql;
        assert!(sql.starts_with("INSERT INTO print_job"));
        // Two value tuples joined => the multi-row form (a per-row loop would
        // record two separate single-tuple executions instead).
        assert!(
            sql.contains("), ("),
            "expected two joined VALUES tuples: {sql}"
        );
    }

    #[test]
    fn apply_reports_whether_the_guarded_update_won() {
        let host = Host::mock();
        host.db.queue_execute_result(1);
        host.db.queue_execute_result(0);
        let claim = Change::Claim {
            station: "st-1",
            printer: Some("main"),
        };
        assert!(apply(&host, 7, &claim, None).unwrap());
        assert!(!apply(&host, 7, &claim, None).unwrap());
        assert!(host.db.executions()[0].sql.starts_with("UPDATE print_job"));
    }

    #[test]
    fn station_listing_offers_exactly_the_claimable_jobs() {
        let host = Host::mock();
        list_station_jobs(&host, None, None, 10).unwrap();
        let mut p = Params::new();
        let claimable = lifecycle::claimable(&mut p);
        assert!(host.db.queries()[0].sql.contains(&claimable));
    }

    #[test]
    fn admin_list_runs_count_then_page() {
        let host = Host::mock();
        host.db.queue_query_result(json!([{ "count": 2 }]));
        host.db.queue_query_result(json!([
            { "id": 1, "user_id": 10, "username": "a", "language": "cpp", "filename": "a.cpp", "status": "pending" },
            { "id": 2, "user_id": 11, "username": "b", "language": "py", "filename": "b.py", "status": "done" }
        ]));
        let filters = JobFilters {
            status: Some(JobStatus::Pending),
            contest_id: Some(1),
            station: None,
            printer: None,
            search: Some("a".into()),
            page: 1,
            per_page: 20,
            sort_by: "created_at".into(),
            sort_order: "desc".into(),
        };
        let (rows, total) = list_admin_jobs(&host, &filters).unwrap();
        assert_eq!(total, 2);
        assert_eq!(rows.len(), 2);
        let queries = host.db.queries();
        assert!(queries[0].sql.contains("COUNT(*)"));
        assert!(queries[1].sql.contains("ILIKE"));
        assert!(queries[1].sql.contains("LIMIT"));
    }
}
