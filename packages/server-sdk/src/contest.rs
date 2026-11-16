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
        "SELECT EXISTS (SELECT 1 FROM submission \
           WHERE contest_id = {} AND status NOT IN {}) AS pending",
        p.bind(contest_id),
        crate::types::StatusPhase::Terminal.sql_list(),
    );
    Ok(host
        .db
        .query_one_with_args::<Pending>(&sql, &p.into_args())?
        .is_some_and(|row| row.pending))
}
