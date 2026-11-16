//! The submission-limit gate's SQL.
//!
//! A slot is held by a committed submission in the `submission` table, or by a
//! reservation one admission took and its submission has not yet made visible
//! there. Counting committed submissions from the table means a submission is
//! counted however late its confirm arrives, or if it never does.
//!
//! Reservations live in one row per (user, problem, contest), in a JSONB
//! object mapping attempt id to when the reservation expires (epoch seconds,
//! the database's `NOW()` plus the attempt timeout the host published on the
//! `before_submission` event). The host inserts no submission for an attempt
//! after that timeout, so a reservation holds a slot exactly while it can still
//! become a submission: until it expires, unless a submission with its attempt
//! id is visible to the counting statement. Both terms are read in one
//! snapshot, so each attempt is counted exactly once: by its reservation until
//! its submission commits, then by the submission.
//!
//! An attempt that ends without a submission (a later hook rejected it, or its
//! insert failed) releases its reservation when the host announces it
//! abandoned, so it holds no slot afterwards; a reservation whose release
//! never arrives still expires with the attempt.
//!
//! Keys are written through `::uuid::text`, so every stored key is a valid
//! UUID and the `::uuid` lookup against `submission.attempt_id` (indexed)
//! never fails on a stored key.
//!
//! Every claim is one `INSERT .. ON CONFLICT DO UPDATE .. WHERE` statement:
//! concurrent claims serialize on the row lock and the `WHERE` re-checks the cap
//! against the row a concurrent winner committed, so two attempts can never
//! both take the last slot. No statement holds a lock across host calls.

use broccoli_server_sdk::prelude::Params;

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

/// Unexpired reservations of `column`, as `jsonb_each_text` rows
/// `e(key, value)`.
fn live(column: &str) -> String {
    format!("jsonb_each_text({column}) e WHERE e.value::float8 > EXTRACT(EPOCH FROM NOW())")
}

/// Reservations of `column` that hold a slot: live, and no submission of
/// their attempt visible to this statement.
fn holding(column: &str) -> String {
    format!(
        "(SELECT COUNT(*) FROM {} AND NOT EXISTS \
           (SELECT 1 FROM submission s WHERE s.attempt_id = e.key::uuid))",
        live(column)
    )
}

/// Take a slot for `attempt_id` until `attempt_timeout_ms` from now if fewer
/// than `max` are held. Affects one row when the reservation was taken and none
/// when the cap is reached. Expired reservations are dropped in the same
/// statement.
pub fn claim_sql(
    p: &mut Params,
    key: &LimitKey,
    attempt_id: &str,
    attempt_timeout_ms: u64,
    max: u32,
) -> String {
    let user = p.bind(key.user_id);
    let problem = p.bind(key.problem_id);
    let contest = p.bind(key.contest_id.unwrap_or(0));
    let attempt = p.bind(attempt_id.to_string());
    let timeout_ms = p.bind(attempt_timeout_ms);
    let committed = key.committed(p);
    let max = p.bind(max);
    let kept = live("submission_limit_claim.reservations");
    let held = holding("submission_limit_claim.reservations");
    format!(
        "INSERT INTO submission_limit_claim (user_id, problem_id, contest_id, reservations) \
         SELECT {user}, {problem}, {contest}, \
           jsonb_build_object({attempt}::uuid::text, \
             EXTRACT(EPOCH FROM NOW()) + {timeout_ms}::float8 / 1000) \
         WHERE {committed} < {max}::bigint \
         ON CONFLICT (user_id, problem_id, contest_id) DO UPDATE SET reservations = \
           (SELECT COALESCE(jsonb_object_agg(e.key, e.value::float8), '{{}}'::jsonb) FROM {kept}) \
           || EXCLUDED.reservations \
         WHERE {committed} + {held} < {max}::bigint"
    )
}

/// Drop `attempt_id`'s reservation on `key`. A committed submission of the
/// attempt keeps its slot through the committed count, so releasing is safe
/// however the attempt ended.
pub fn release_sql(p: &mut Params, key: &LimitKey, attempt_id: &str) -> String {
    let attempt = p.bind(attempt_id.to_string());
    format!(
        "UPDATE submission_limit_claim SET reservations = reservations - {attempt}::uuid::text \
         WHERE {}",
        key.claim_row(p, "submission_limit_claim")
    )
}

/// The slots `key` holds, as one `count` column. Reads the same terms the claim
/// enforces.
pub fn held_count_sql(p: &mut Params, key: &LimitKey) -> String {
    let committed = key.committed(p);
    let held = holding("c.reservations");
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
        let sql = claim_sql(&mut p, &KEY, "a1", 60_000, 3);
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
    fn a_release_drops_only_that_attempts_reservation() {
        let mut p = Params::new();
        let sql = release_sql(&mut p, &KEY, "a1");
        assert!(sql.contains("reservations - $1::uuid::text"), "{sql}");
        assert!(
            sql.contains("submission_limit_claim.contest_id = $4"),
            "{sql}"
        );
        assert_eq!(p.into_args()[0], serde_json::json!("a1"));
    }

    #[test]
    fn every_window_bound_comes_from_the_database_clock() {
        for sql in [
            claim_sql(&mut Params::new(), &KEY, "a", 60_000, 1),
            held_count_sql(&mut Params::new(), &KEY),
        ] {
            assert!(sql.contains("EXTRACT(EPOCH FROM NOW())"), "{sql}");
        }
    }
}
