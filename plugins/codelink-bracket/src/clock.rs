//! Server-side time and contest bounds. Every comparison against a time
//! written by another node uses the database clock, never the local one.

use broccoli_server_sdk::Host;
use broccoli_server_sdk::db::Params;
use broccoli_server_sdk::error::SdkError;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ContestTimesRow {
    start_ms: f64,
    #[serde(default)]
    end_ms: Option<f64>,
}

/// A contest's start and end, epoch ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContestTimes {
    pub start_ms: i64,
    pub end_ms: Option<i64>,
}

/// The contest's start time, epoch ms. Round 1 cannot start before it.
pub(crate) fn contest_times(host: &Host, contest: i32) -> Result<ContestTimes, SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "SELECT EXTRACT(EPOCH FROM start_time) * 1000 AS start_ms, \
         EXTRACT(EPOCH FROM end_time) * 1000 AS end_ms FROM contest WHERE id = {}",
        p.bind(contest)
    );
    let row: Option<ContestTimesRow> = host.db.query_one_with_args(&sql, &p.into_args())?;
    row.map(|r| ContestTimes {
        start_ms: r.start_ms as i64,
        end_ms: r.end_ms.map(|e| e as i64),
    })
    .ok_or_else(|| SdkError::Other("contest not found".into()))
}

#[derive(Debug, Deserialize)]
struct NowRow {
    now_ms: f64,
}

/// The current server time in Unix epoch milliseconds, via a `NOW()` query
/// -- following the codebase-wide `EXTRACT(EPOCH FROM ...) * 1000` raw-SQL
/// convention for epoch-ms columns (see `plugins/icpc/src/lib.rs`,
/// `plugins/cooldown/src/lib.rs`), which deserializes as `f64`, not `i64`.
pub(crate) fn now_ms(host: &Host) -> Result<i64, SdkError> {
    let row: Option<NowRow> = host
        .db
        .query_one("SELECT EXTRACT(EPOCH FROM NOW()) * 1000 AS now_ms")?;
    let row = row.ok_or_else(|| SdkError::Other("NOW() query returned no row".into()))?;
    Ok(row.now_ms as i64)
}

#[derive(Debug, Deserialize)]
struct ClockRow {
    now_ms: f64,
    #[serde(default)]
    end_ms: Option<f64>,
}

/// The server time together with the contest's end time, both epoch ms.
/// Nobody can submit once the contest has ended, so no game may open or
/// run past `contest_end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Clock {
    pub now: i64,
    /// `None` only if the contest row could not be read.
    pub contest_end: Option<i64>,
}

impl Clock {
    #[cfg(test)]
    pub(crate) fn at(now: i64) -> Self {
        Self {
            now,
            contest_end: None,
        }
    }

    pub(crate) fn contest_ended(&self) -> bool {
        self.contest_end.is_some_and(|end| self.now >= end)
    }

    /// When a game with this deadline must be looked at again: its own
    /// deadline, or the contest end if that comes first.
    pub(crate) fn cutoff(&self, deadline_ms: i64) -> i64 {
        self.contest_end
            .map_or(deadline_ms, |end| deadline_ms.min(end))
    }
}

/// [`now_ms`] and the contest end in one round trip.
pub(crate) fn clock(host: &Host, contest: i32) -> Result<Clock, SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "SELECT EXTRACT(EPOCH FROM NOW()) * 1000 AS now_ms, \
         (SELECT EXTRACT(EPOCH FROM end_time) * 1000 FROM contest WHERE id = {}) AS end_ms",
        p.bind(contest)
    );
    let row: Option<ClockRow> = host.db.query_one_with_args(&sql, &p.into_args())?;
    let row = row.ok_or_else(|| SdkError::Other("NOW() query returned no row".into()))?;
    Ok(Clock {
        now: row.now_ms as i64,
        contest_end: row.end_ms.map(|e| e as i64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::queue_now;

    #[test]
    fn now_ms_reads_the_queued_row() {
        let host = Host::mock();
        queue_now(&host, 1_234_567);
        assert_eq!(now_ms(&host).unwrap(), 1_234_567);
    }

    // -- open_xiaoju --
}
