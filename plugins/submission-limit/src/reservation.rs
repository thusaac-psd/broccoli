//! The submission-limit gate's SQL.
//!
//! A slot is held by a committed submission in the `submission` table, or by a
//! reservation one admission took and its submission has not yet made visible
//! there. Counting committed submissions from the table means a submission is
//! counted however late its confirm arrives, or if it never does.
//!
//! Reservations live in one row per (user, problem, contest), in a JSONB
//! object mapping attempt id to the database's `NOW()` (epoch seconds) when the
//! reservation was taken. A reservation holds a slot while it is younger than
//! [`RESERVATION_GRACE_SECS`] and no submission with its attempt id is visible
//! to the counting statement. Both terms are read in one snapshot, so each
//! attempt is counted exactly once: by its reservation until its submission
//! commits, then by the submission. An attempt that never commits frees its
//! slot when the grace ends.
//!
//! Every claim is one `INSERT .. ON CONFLICT DO UPDATE .. WHERE` statement:
//! concurrent claims serialize on the row lock and the `WHERE` re-checks the cap
//! against the row a concurrent winner committed, so two attempts can never
//! both take the last slot. No statement holds a lock across host calls.

use broccoli_server_sdk::prelude::Params;

/// Seconds a reservation holds a slot without its submission being visible.
/// Covers the time from admission to the submission's commit; an attempt
/// rejected by a later hook or a failed insert costs its user a slot this long.
pub const RESERVATION_GRACE_SECS: i64 = 30;

pub const SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS submission_limit_claim (\
     user_id      INTEGER NOT NULL, \
     problem_id   INTEGER NOT NULL, \
     contest_id   INTEGER NOT NULL DEFAULT 0, \
     reservations JSONB   NOT NULL DEFAULT '{}'::jsonb, \
     PRIMARY KEY (user_id, problem_id, contest_id))";

/// The (user, problem, contest) a limit applies to.
#[derive(Debug, Clone, Copy)]
pub struct LimitKey {
    pub user_id: i32,
    pub problem_id: i32,
    pub contest_id: Option<i32>,
}

impl LimitKey {
    /// `WHERE` terms selecting this key's claim row in `table`. The row's
    /// `contest_id` is 0 for a submission outside any contest.
    fn claim_row(&self, p: &mut Params, table: &str) -> String {
        format!(
            "{table}.user_id = {} AND {table}.problem_id = {} AND {table}.contest_id = {}",
            p.bind(self.user_id),
            p.bind(self.problem_id),
            p.bind(self.contest_id.unwrap_or(0))
        )
    }

    /// Submissions this key has committed.
    fn committed(&self, p: &mut Params) -> String {
        let contest = match self.contest_id {
            Some(cid) => format!("s.contest_id = {}", p.bind(cid)),
            None => "s.contest_id IS NULL".to_string(),
        };
        format!(
            "(SELECT COUNT(*) FROM submission s \
              WHERE s.user_id = {} AND s.problem_id = {} AND {contest})",
            p.bind(self.user_id),
            p.bind(self.problem_id)
        )
    }
}

/// Reservations of `column` still inside the grace window, as `jsonb_each_text`
/// rows `e(key, value)`.
fn live(p: &mut Params, column: &str) -> String {
    format!(
        "jsonb_each_text({column}) e \
         WHERE e.value::float8 > EXTRACT(EPOCH FROM NOW()) - {}::float8",
        p.bind(RESERVATION_GRACE_SECS)
    )
}

/// Reservations of `column` that hold a slot: live, and no submission of
/// their attempt visible to this statement.
fn holding(p: &mut Params, column: &str) -> String {
    format!(
        "(SELECT COUNT(*) FROM {} AND NOT EXISTS \
           (SELECT 1 FROM submission s WHERE s.attempt_id = e.key::uuid))",
        live(p, column)
    )
}

/// Take a slot for `attempt_id` if fewer than `max` are held. Affects one row
/// when the reservation was taken and none when the cap is reached. Expired
/// reservations are dropped in the same statement.
pub fn claim_sql(p: &mut Params, key: &LimitKey, attempt_id: &str, max: u32) -> String {
    let user = p.bind(key.user_id);
    let problem = p.bind(key.problem_id);
    let contest = p.bind(key.contest_id.unwrap_or(0));
    let attempt = p.bind(attempt_id.to_string());
    let committed = key.committed(p);
    let max = p.bind(max);
    let kept = live(p, "submission_limit_claim.reservations");
    let held = holding(p, "submission_limit_claim.reservations");
    format!(
        "INSERT INTO submission_limit_claim (user_id, problem_id, contest_id, reservations) \
         SELECT {user}, {problem}, {contest}, \
           jsonb_build_object({attempt}::text, EXTRACT(EPOCH FROM NOW())) \
         WHERE {committed} < {max}::bigint \
         ON CONFLICT (user_id, problem_id, contest_id) DO UPDATE SET reservations = \
           (SELECT COALESCE(jsonb_object_agg(e.key, e.value::float8), '{{}}'::jsonb) FROM {kept}) \
           || EXCLUDED.reservations \
         WHERE {committed} + {held} < {max}::bigint"
    )
}

/// The slots `key` holds, as one `count` column. Reads the same terms the claim
/// enforces.
pub fn held_count_sql(p: &mut Params, key: &LimitKey) -> String {
    let committed = key.committed(p);
    let held = holding(p, "c.reservations");
    format!(
        "SELECT ({committed} + COALESCE((SELECT {held} FROM submission_limit_claim c WHERE {}), 0))::bigint AS count",
        key.claim_row(p, "c")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: LimitKey = LimitKey {
        user_id: 1,
        problem_id: 2,
        contest_id: None,
    };

    #[test]
    fn a_claim_counts_committed_submissions_and_live_unconfirmed_reservations() {
        let mut p = Params::new();
        let sql = claim_sql(&mut p, &KEY, "a1", 3);
        assert!(sql.contains("FROM submission s"), "{sql}");
        assert!(sql.contains("s.contest_id IS NULL"), "{sql}");
        assert!(sql.contains("s.attempt_id = e.key::uuid"), "{sql}");
        assert!(sql.contains("ON CONFLICT"), "{sql}");
        assert!(
            p.into_args().contains(&serde_json::json!("a1")),
            "the attempt id is bound, not inlined"
        );
    }

    #[test]
    fn every_window_bound_comes_from_the_database_clock() {
        for sql in [
            claim_sql(&mut Params::new(), &KEY, "a", 1),
            held_count_sql(&mut Params::new(), &KEY),
        ] {
            assert!(sql.contains("EXTRACT(EPOCH FROM NOW())"), "{sql}");
        }
    }
}
