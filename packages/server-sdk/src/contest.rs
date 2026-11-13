use serde::Deserialize;

use crate::api::ApiError;
#[cfg(feature = "guest")]
use crate::error::SdkError;
#[cfg(feature = "guest")]
use crate::types::PluginHttpRequest;
use crate::types::PluginHttpResponse;
#[cfg(feature = "guest")]
use broccoli_types::permissions as perm;

/// SQL yielding a contest's phase, `'before'`, `'during'` or `'after'`, for the
/// contest row aliased `table` (empty for an unaliased `contest`). A contest
/// runs from `start_time` up to but excluding `end_time`, the host's boundary,
/// on the database clock.
pub fn phase_sql(table: &str) -> String {
    let col = |name: &str| {
        if table.is_empty() {
            name.to_string()
        } else {
            format!("{table}.{name}")
        }
    };
    format!(
        "CASE WHEN NOW() < {} THEN 'before' WHEN NOW() >= {} THEN 'after' ELSE 'during' END",
        col("start_time"),
        col("end_time")
    )
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContestInfo {
    pub contest_type: Option<String>,
    pub is_public: bool,
    pub is_active: bool,
    pub phase: String,
}

impl ContestInfo {
    pub fn require_type(&self, expected: &str) -> Result<(), ApiError> {
        if self.contest_type.as_deref() == Some(expected) {
            Ok(())
        } else {
            Err(PluginHttpResponse::error(404, format!("Not a {expected} contest")).into())
        }
    }
}

#[cfg(feature = "guest")]
pub fn load_info(host: &crate::sdk::Host, contest_id: i32) -> Result<ContestInfo, ApiError> {
    let mut p = crate::db::Params::new();
    let sql = format!(
        "SELECT contest_type, is_public, \
            ((activate_time IS NULL OR activate_time <= NOW()) AND \
             (deactivate_time IS NULL OR deactivate_time > NOW())) AS is_active, \
            {} AS phase \
         FROM contest WHERE id = {}",
        phase_sql(""),
        p.bind(contest_id)
    );
    host.db
        .query_one_with_args::<ContestInfo>(&sql, &p.into_args())
        .map_err(ApiError::from)?
        .ok_or_else(|| PluginHttpResponse::error(404, "Contest not found").into())
}

#[cfg(feature = "guest")]
pub fn check_access(
    host: &crate::sdk::Host,
    req: &PluginHttpRequest,
    contest_id: i32,
) -> Result<ContestInfo, ApiError> {
    let info = load_info(host, contest_id)?;
    if !can_view(host, req, contest_id, &info)? {
        return Err(PluginHttpResponse::error(404, "Contest not found").into());
    }
    Ok(info)
}

#[cfg(feature = "guest")]
fn can_view(
    host: &crate::sdk::Host,
    req: &PluginHttpRequest,
    contest_id: i32,
    info: &ContestInfo,
) -> Result<bool, ApiError> {
    if req.has_permission(perm::CONTEST_MANAGE) {
        return Ok(true);
    }
    if !info.is_active {
        return Ok(false);
    }
    if info.is_public {
        return Ok(true);
    }
    match req.user_id() {
        Some(user_id) => is_participant(host, contest_id, user_id).map_err(ApiError::from),
        None => Ok(false),
    }
}

#[cfg(feature = "guest")]
pub fn is_participant(
    host: &crate::sdk::Host,
    contest_id: i32,
    user_id: i32,
) -> Result<bool, SdkError> {
    #[derive(Deserialize)]
    struct ExistsRow {
        exists: bool,
    }

    let mut p = crate::db::Params::new();
    let sql = format!(
        "SELECT EXISTS( \
            SELECT 1 FROM contest_user \
            WHERE contest_id = {} AND user_id = {} \
         ) AS exists",
        p.bind(contest_id),
        p.bind(user_id)
    );
    Ok(host
        .db
        .query_one_with_args::<ExistsRow>(&sql, &p.into_args())?
        .is_some_and(|row| row.exists))
}

#[cfg(feature = "guest")]
pub fn has_problem(
    host: &crate::sdk::Host,
    contest_id: i32,
    problem_id: i32,
) -> Result<bool, SdkError> {
    #[derive(Deserialize)]
    struct ExistsRow {
        exists: bool,
    }

    let mut p = crate::db::Params::new();
    let sql = format!(
        "SELECT EXISTS( \
            SELECT 1 FROM contest_problem \
            WHERE contest_id = {} AND problem_id = {} \
         ) AS exists",
        p.bind(contest_id),
        p.bind(problem_id)
    );
    Ok(host
        .db
        .query_one_with_args::<ExistsRow>(&sql, &p.into_args())?
        .is_some_and(|row| row.exists))
}

#[cfg(feature = "guest")]
pub fn check_problem_access(
    host: &crate::sdk::Host,
    req: &PluginHttpRequest,
    contest_id: i32,
    user_id: i32,
    problem_id: i32,
) -> Result<(), ApiError> {
    let info = load_info(host, contest_id)?;

    if !req.has_permission(perm::CONTEST_MANAGE)
        && (!info.is_active || !is_participant(host, contest_id, user_id)?)
    {
        return Err(PluginHttpResponse::error(404, "Contest not found").into());
    }

    if !has_problem(host, contest_id, problem_id)? {
        return Err(PluginHttpResponse::error(404, "Contest not found").into());
    }

    Ok(())
}

#[cfg(feature = "guest")]
pub fn load_config<T: serde::de::DeserializeOwned + Default>(
    host: &crate::sdk::Host,
    contest_id: i32,
) -> Result<T, SdkError> {
    Ok(
        serde_json::from_value(host.config.get_contest(contest_id, "contest")?.config)
            .unwrap_or_default(),
    )
}
