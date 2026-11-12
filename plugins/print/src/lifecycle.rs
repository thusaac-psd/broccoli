//! The print job state machine. [`JobStatus`] names every state, [`Change`]
//! names every move together with the data it writes, and [`Change::edge`] is
//! the one table of which states each move may leave and where it lands.
//! [`transition_sql`] turns a move into the single guarded `UPDATE` that
//! performs it, so no write path can change `status` without going through the
//! table.

use broccoli_server_sdk::prelude::Params;
use serde::{Deserialize, Serialize};

/// A claim that has not reached `done` or `failed` within this many seconds
/// belongs to a station that died. Any station may then claim the job again,
/// and the original station can no longer report on it.
pub const CLAIM_LEASE_SECS: i64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    PendingApproval,
    Pending,
    Claimed,
    Printing,
    Done,
    Failed,
    Canceled,
}

impl JobStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PendingApproval => "pending_approval",
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Printing => "printing",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }

    /// The state a new job starts in.
    pub fn initial(require_approval: bool) -> Self {
        if require_approval {
            Self::PendingApproval
        } else {
            Self::Pending
        }
    }
}

/// What a station reports about a job it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StationReport {
    Printing,
    Done,
    Failed,
}

/// One move of a job, with everything that move writes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change<'a> {
    /// Staff releases a job held for approval to the stations.
    Approve,
    /// Staff withdraws a job that has not printed.
    Cancel,
    /// Staff sends a finished job back to the queue.
    Reprint,
    /// Staff routes a queued or finished job to a printer (or any printer)
    /// and queues it.
    Pin { printer: Option<&'a str> },
    /// A station takes a queued job, or one whose claim lease expired.
    Claim {
        station: &'a str,
        printer: Option<&'a str>,
    },
    /// The station holding the job reports progress.
    Report {
        station: &'a str,
        report: StationReport,
        pages: Option<i32>,
        error: Option<&'a str>,
    },
}

use JobStatus::*;

impl Change<'_> {
    /// The states this move may leave, and the state it lands in.
    pub fn edge(&self) -> (&'static [JobStatus], JobStatus) {
        match self {
            Change::Approve => (&[PendingApproval], Pending),
            Change::Cancel => (
                &[PendingApproval, Pending, Claimed, Printing, Failed],
                Canceled,
            ),
            Change::Reprint => (&[Done, Failed], Pending),
            Change::Pin { .. } => (&[Pending, Done, Failed], Pending),
            Change::Claim { .. } => (&[Pending], Claimed),
            Change::Report { report, .. } => match report {
                StationReport::Printing => (&[Claimed], Printing),
                StationReport::Done => (&[Claimed, Printing], Done),
                StationReport::Failed => (&[Claimed, Printing], Failed),
            },
        }
    }

    /// The column writes for this move besides `status`. Entering the queue
    /// clears the previous run, and entering `claimed` starts a new one, so a
    /// job never carries a claim, page count or error from an earlier attempt.
    fn assignments(&self, p: &mut Params) -> Vec<String> {
        const CLEAR_RUN: [&str; 6] = [
            "claimed_by = NULL",
            "claimed_printer = NULL",
            "claimed_at = NULL",
            "printed_at = NULL",
            "pages = NULL",
            "error = NULL",
        ];
        let mut sets: Vec<String> = Vec::new();
        match *self {
            Change::Approve | Change::Reprint => sets.extend(CLEAR_RUN.map(String::from)),
            Change::Cancel => {}
            Change::Pin { printer } => {
                sets.push(format!(
                    "target_printer = {}",
                    bind_opt(p, printer.filter(|s| !s.is_empty()))
                ));
                sets.extend(CLEAR_RUN.map(String::from));
            }
            Change::Claim { station, printer } => {
                sets.push(format!("claimed_by = {}", p.bind(station.to_string())));
                sets.push(format!("claimed_printer = {}", bind_opt(p, printer)));
                sets.push("claimed_at = NOW()".to_string());
                sets.extend(CLEAR_RUN[3..].iter().map(|s| s.to_string()));
            }
            Change::Report {
                report,
                pages,
                error,
                ..
            } => {
                if report != StationReport::Printing {
                    sets.push("printed_at = NOW()".to_string());
                    sets.push(format!("pages = {}", bind_opt(p, pages)));
                    sets.push(format!("error = {}", bind_opt(p, error)));
                }
            }
        }
        sets
    }

    /// Which rows in the source states this move may touch. A claim may also
    /// take over a job whose lease expired; a report must come from the
    /// station that holds an unexpired claim.
    fn guard(&self, p: &mut Params) -> String {
        let (from, _) = self.edge();
        let in_from = format!(
            "status IN ({})",
            from.iter()
                .map(|s| p.bind(s.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        );
        match *self {
            Change::Claim { .. } => format!("({in_from} OR {})", lease_expired(p)),
            Change::Report { station, .. } => format!(
                "{in_from} AND claimed_by = {} AND NOT {}",
                p.bind(station.to_string()),
                lease_expired(p)
            ),
            _ => in_from,
        }
    }
}

/// A held job whose claim is older than the lease.
fn lease_expired(p: &mut Params) -> String {
    format!(
        "(status IN ({}, {}) AND claimed_at <= NOW() - ({} * INTERVAL '1 second'))",
        p.bind(Claimed.as_str()),
        p.bind(Printing.as_str()),
        p.bind(CLAIM_LEASE_SECS)
    )
}

/// `None` emits a literal NULL rather than a bound parameter.
pub(crate) fn bind_opt<T: Into<serde_json::Value>>(p: &mut Params, v: Option<T>) -> String {
    match v {
        Some(v) => p.bind(v),
        None => "NULL".to_string(),
    }
}

/// The predicate selecting jobs a station may claim now, for listing.
pub(crate) fn claimable(p: &mut Params) -> String {
    Change::Claim {
        station: "",
        printer: None,
    }
    .guard(p)
}

/// The guarded `UPDATE` performing `change` on job `id`. `contest_filter`
/// confines it to one contest. It affects one row when the move is allowed and
/// none otherwise.
pub fn transition_sql(
    p: &mut Params,
    id: i64,
    change: &Change<'_>,
    contest_filter: Option<i32>,
) -> String {
    let (_, to) = change.edge();
    let mut sets = vec![format!("status = {}", p.bind(to.as_str()))];
    sets.extend(change.assignments(p));
    let mut sql = format!(
        "UPDATE print_job SET {} WHERE id = {} AND {}",
        sets.join(", "),
        p.bind(id),
        change.guard(p)
    );
    if let Some(cid) = contest_filter {
        sql.push_str(&format!(" AND contest_id = {}", p.bind(cid)));
    }
    sql
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [JobStatus; 7] = [
        PendingApproval,
        Pending,
        Claimed,
        Printing,
        Done,
        Failed,
        Canceled,
    ];

    fn allowed(change: Change<'_>, from: JobStatus) -> bool {
        change.edge().0.contains(&from)
    }

    #[test]
    fn status_strings_round_trip() {
        for status in ALL {
            assert_eq!(JobStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(JobStatus::parse("bogus"), None);
    }

    #[test]
    fn requeueing_moves_never_bypass_approval_or_revive_canceled_or_in_flight_jobs() {
        let pin = Change::Pin { printer: None };
        for change in [Change::Reprint, pin] {
            for from in [PendingApproval, Canceled, Claimed, Printing] {
                assert!(!allowed(change, from), "{change:?} from {from:?}");
            }
        }
        assert!(allowed(Change::Reprint, Done));
        assert!(allowed(Change::Reprint, Failed));
        assert!(allowed(pin, Pending));
    }

    #[test]
    fn only_approval_releases_a_job_held_for_approval() {
        let staff_moves = [
            Change::Approve,
            Change::Cancel,
            Change::Reprint,
            Change::Pin { printer: None },
        ];
        let releases: Vec<_> = staff_moves
            .into_iter()
            .filter(|c| allowed(*c, PendingApproval) && c.edge().1 != Canceled)
            .collect();
        assert_eq!(releases, vec![Change::Approve]);
    }

    #[test]
    fn done_jobs_cannot_be_canceled_and_canceled_jobs_stay_canceled() {
        assert!(!allowed(Change::Cancel, Done));
        for change in [
            Change::Approve,
            Change::Reprint,
            Change::Pin { printer: None },
            Change::Claim {
                station: "s",
                printer: None,
            },
        ] {
            assert!(!allowed(change, Canceled), "{change:?}");
        }
    }

    #[test]
    fn a_report_requires_the_reporting_station_to_hold_the_claim() {
        let mut p = Params::new();
        let change = Change::Report {
            station: "room-a",
            report: StationReport::Done,
            pages: Some(2),
            error: None,
        };
        let sql = transition_sql(&mut p, 7, &change, None);
        assert!(sql.contains("claimed_by = $"), "{sql}");
        assert!(sql.contains("AND NOT (status IN"), "{sql}");
        assert!(sql.contains("printed_at = NOW()"), "{sql}");
        let args = p.into_args();
        assert!(args.contains(&serde_json::json!("room-a")));
        assert!(args.contains(&serde_json::json!("done")));
    }

    #[test]
    fn a_printing_report_does_not_finish_the_run() {
        let mut p = Params::new();
        let change = Change::Report {
            station: "room-a",
            report: StationReport::Printing,
            pages: None,
            error: None,
        };
        let sql = transition_sql(&mut p, 7, &change, None);
        assert!(!sql.contains("printed_at"), "{sql}");
    }

    #[test]
    fn a_claim_starts_a_fresh_run_and_may_take_over_an_expired_lease() {
        let mut p = Params::new();
        let change = Change::Claim {
            station: "room-b",
            printer: Some("main"),
        };
        let sql = transition_sql(&mut p, 7, &change, Some(3));
        assert!(sql.contains("claimed_at = NOW()"), "{sql}");
        assert!(sql.contains("printed_at = NULL"), "{sql}");
        assert!(sql.contains("pages = NULL"), "{sql}");
        assert!(sql.contains("error = NULL"), "{sql}");
        assert!(sql.contains(" OR (status IN"), "{sql}");
        assert!(sql.contains("claimed_at <= NOW() -"), "{sql}");
        assert!(sql.contains("AND contest_id = $"), "{sql}");
    }

    #[test]
    fn requeueing_clears_every_field_of_the_previous_run() {
        for change in [Change::Reprint, Change::Pin { printer: Some("x") }] {
            let mut p = Params::new();
            let sql = transition_sql(&mut p, 1, &change, None);
            for col in [
                "claimed_by",
                "claimed_printer",
                "claimed_at",
                "printed_at",
                "pages",
                "error",
            ] {
                assert!(sql.contains(&format!("{col} = NULL")), "{change:?}: {sql}");
            }
        }
    }
}
