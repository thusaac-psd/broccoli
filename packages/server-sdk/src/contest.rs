//! Contest helpers for plugins. A route's contest, its phase and the
//! caller's access to it come from the host (`PluginHttpRequest::contest`),
//! never from plugin SQL.

#[cfg(feature = "guest")]
use crate::error::SdkError;

/// The contest's `contest` config namespace as the plugin's config type, with
/// the schema's defaults filled in by the host.
#[cfg(feature = "guest")]
pub fn load_config<T: serde::de::DeserializeOwned>(
    host: &crate::sdk::Host,
    contest_id: i32,
) -> Result<T, SdkError> {
    host.config.get_contest(contest_id, "contest")?.parse()
}

/// The SQL condition that submission row `alias` holds its standing result:
/// its status is terminal. The host projects a submission's current
/// judgement onto its row, but keeps a result that may still change (one in
/// flight, or a system error the retry may still take back) at a status that
/// is not terminal, so a view that reads what a submission scored reads only
/// rows this holds for.
pub fn standing_result(alias: &str) -> String {
    format!(
        "{alias}.status IN {}",
        crate::types::StatusPhase::Terminal.sql_list()
    )
}

/// Whether a view computed from the contest's results, such as its board, may
/// still change: the contest has not ended, or one of its submissions is
/// still being judged (its status is not terminal), as a submission made in
/// the last minute is after the end. A board polls while this holds; it reads
/// the database only once the contest has ended.
#[cfg(feature = "guest")]
pub fn results_may_change(
    host: &crate::sdk::Host,
    contest_id: i32,
    phase: crate::types::ContestPhase,
) -> Result<bool, SdkError> {
    if !phase.has_ended() {
        return Ok(true);
    }
    #[derive(serde::Deserialize)]
    struct Pending {
        pending: bool,
    }
    let mut p = crate::db::Params::new();
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM submission s \
           WHERE s.contest_id = {} AND NOT ({})) AS pending",
        p.bind(contest_id),
        standing_result("s"),
    );
    Ok(host
        .db
        .query_one_with_args::<Pending>(&sql, &p.into_args())?
        .is_some_and(|row| row.pending))
}

/// One problem of a contest, as the contest names it.
#[cfg(feature = "guest")]
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ContestProblem {
    pub problem_id: i32,
    /// The contest's label for the problem, which the host requires and
    /// every view of the contest shows.
    pub label: String,
}

/// The contest's problems in the contest's order, each with the label the
/// contest gives it. A board names its columns by these labels, as the host's
/// own contest pages do.
#[cfg(feature = "guest")]
pub fn problems(host: &crate::sdk::Host, contest_id: i32) -> Result<Vec<ContestProblem>, SdkError> {
    let mut p = crate::db::Params::new();
    let sql = format!(
        "SELECT problem_id, label FROM contest_problem WHERE contest_id = {} ORDER BY position",
        p.bind(contest_id)
    );
    host.db.query_with_args(&sql, &p.into_args())
}
