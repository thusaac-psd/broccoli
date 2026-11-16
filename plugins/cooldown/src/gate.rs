//! The cooldown gate: its SQL and the one rule for how long a user must wait.
//!
//! Two clocks block a submission. The durable one runs from the user's last
//! committed submission for the (user, problem, contest) and lasts the full
//! cooldown. The claim closes the gap while a submission is in flight: the hook
//! passes before the server inserts the row, so concurrent attempts would
//! otherwise all see the old last submission. Each admission claims the slot in
//! `cooldown_claim` until the attempt can no longer become a submission (the
//! attempt timeout the host publishes on `before_submission`), capped at the
//! cooldown, which the committed row then enforces. The claim records the
//! attempt that took it, and an attempt that ends without a submission (a
//! later hook rejected it, or its insert failed) releases its own claim when
//! the host announces it abandoned, so it blocks nothing afterwards; a claim
//! whose release never arrives still ends with the attempt timeout. Every
//! time is the database's `NOW()`, and `submission.created_at` is written by
//! the database too.

use broccoli_server_sdk::prelude::Params;

pub const SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS cooldown_claim (\
     user_id       INTEGER NOT NULL, \
     problem_id    INTEGER NOT NULL, \
     contest_id    INTEGER NOT NULL DEFAULT 0, \
     blocked_until TIMESTAMPTZ NOT NULL, \
     attempt_id    UUID        NOT NULL, \
     PRIMARY KEY (user_id, problem_id, contest_id))";

/// Each clock's state in seconds: since the last committed submission, and
/// until the claim stops blocking. `None` when there is no such row.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Deserialize)]
pub struct Clocks {
    pub since_submission: Option<f64>,
    pub claim_remaining: Option<f64>,
}

impl Clocks {
    /// Whole seconds until a submission may pass, 0 when it may now.
    pub fn wait_secs(&self, cooldown: u32) -> u64 {
        let durable = self
            .since_submission
            .map_or(0.0, |s| f64::from(cooldown) - s.max(0.0));
        let claim = self.claim_remaining.unwrap_or(0.0);
        durable.max(claim).max(0.0).ceil() as u64
    }
}

/// The (user, problem, contest) a cooldown applies to.
#[derive(Debug, Clone, Copy)]
pub struct CooldownKey {
    pub user_id: i32,
    pub problem_id: i32,
    pub contest_id: Option<i32>,
}

impl CooldownKey {
    /// A committed submission of this key within the last `cooldown` seconds.
    fn recent_submission(&self, p: &mut Params, cooldown: u32) -> String {
        format!(
            "EXISTS (SELECT 1 FROM submission s WHERE {} \
               AND s.created_at > NOW() - ({}::int * INTERVAL '1 second'))",
            self.submission_rows(p),
            p.bind(cooldown)
        )
    }

    fn submission_rows(&self, p: &mut Params) -> String {
        let contest = match self.contest_id {
            Some(cid) => format!("s.contest_id = {}", p.bind(cid)),
            None => "s.contest_id IS NULL".to_string(),
        };
        format!(
            "s.user_id = {} AND s.problem_id = {} AND {contest}",
            p.bind(self.user_id),
            p.bind(self.problem_id)
        )
    }

    /// The key's claim row; `contest_id` is 0 outside any contest.
    fn claim_row(&self, p: &mut Params, table: &str) -> String {
        format!(
            "{table}.user_id = {} AND {table}.problem_id = {} AND {table}.contest_id = {}",
            p.bind(self.user_id),
            p.bind(self.problem_id),
            p.bind(self.contest_id.unwrap_or(0))
        )
    }
}

/// Claim the slot when neither clock blocks, until `attempt_timeout_ms` from
/// now or the cooldown, whichever is shorter. Affects one row when the
/// submission may pass and none otherwise, in one statement: concurrent claims
/// serialize on the row and the `WHERE` re-checks against the claim a
/// concurrent winner committed.
pub fn claim_sql(
    p: &mut Params,
    key: &CooldownKey,
    attempt_id: &str,
    cooldown: u32,
    attempt_timeout_ms: u64,
) -> String {
    let user = p.bind(key.user_id);
    let problem = p.bind(key.problem_id);
    let contest = p.bind(key.contest_id.unwrap_or(0));
    let attempt = p.bind(attempt_id.to_string());
    let recent = key.recent_submission(p, cooldown);
    let cooldown_ms = p.bind(u64::from(cooldown) * 1000);
    let timeout_ms = p.bind(attempt_timeout_ms);
    format!(
        "INSERT INTO cooldown_claim (user_id, problem_id, contest_id, blocked_until, attempt_id) \
         SELECT {user}, {problem}, {contest}, \
           NOW() + LEAST({cooldown_ms}::bigint, {timeout_ms}::bigint) * INTERVAL '1 millisecond', \
           {attempt}::uuid \
         WHERE NOT {recent} \
         ON CONFLICT (user_id, problem_id, contest_id) \
         DO UPDATE SET blocked_until = EXCLUDED.blocked_until, attempt_id = EXCLUDED.attempt_id \
         WHERE cooldown_claim.blocked_until <= NOW() AND NOT {recent}"
    )
}

/// End the claim `attempt_id` holds on `key`, if it still holds it. A
/// committed submission of the attempt keeps blocking through the durable
/// clock, so releasing is safe however the attempt ended.
pub fn release_sql(p: &mut Params, key: &CooldownKey, attempt_id: &str) -> String {
    let claim = key.claim_row(p, "cooldown_claim");
    format!(
        "UPDATE cooldown_claim SET blocked_until = LEAST(blocked_until, NOW()) \
         WHERE {claim} AND attempt_id = {}::uuid",
        p.bind(attempt_id.to_string())
    )
}

/// Both clocks for `key`, as one [`Clocks`] row.
pub fn clocks_sql(p: &mut Params, key: &CooldownKey) -> String {
    let submissions = key.submission_rows(p);
    let claim = key.claim_row(p, "c");
    format!(
        "SELECT \
           (SELECT EXTRACT(EPOCH FROM (NOW() - MAX(s.created_at)))::float8 \
              FROM submission s WHERE {submissions}) AS since_submission, \
           (SELECT EXTRACT(EPOCH FROM (c.blocked_until - NOW()))::float8 \
              FROM cooldown_claim c WHERE {claim}) AS claim_remaining"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: CooldownKey = CooldownKey {
        user_id: 1,
        problem_id: 2,
        contest_id: None,
    };

    fn clocks(since_submission: Option<f64>, claim_remaining: Option<f64>) -> Clocks {
        Clocks {
            since_submission,
            claim_remaining,
        }
    }

    #[test]
    fn nothing_blocks_a_first_submission() {
        assert_eq!(clocks(None, None).wait_secs(60), 0);
    }

    #[test]
    fn the_durable_clock_runs_the_full_cooldown_from_the_last_submission() {
        assert_eq!(clocks(Some(20.0), Some(-5.0)).wait_secs(60), 40);
        assert_eq!(clocks(Some(59.2), None).wait_secs(60), 1);
        assert_eq!(clocks(Some(60.0), None).wait_secs(60), 0);
    }

    #[test]
    fn an_unexpired_claim_blocks_until_it_expires() {
        assert_eq!(clocks(None, Some(9.5)).wait_secs(60), 10);
        assert_eq!(clocks(None, Some(-1.0)).wait_secs(60), 0);
    }

    #[test]
    fn the_longer_of_the_two_clocks_wins() {
        assert_eq!(clocks(Some(55.0), Some(13.0)).wait_secs(60), 13);
        assert_eq!(clocks(Some(30.0), Some(13.0)).wait_secs(60), 30);
    }

    #[test]
    fn a_claim_records_its_attempt_and_only_that_attempt_releases_it() {
        let mut p = Params::new();
        let claim = claim_sql(&mut p, &KEY, "a1", 60, 60_000);
        assert!(
            claim.contains("attempt_id = EXCLUDED.attempt_id"),
            "{claim}"
        );
        assert!(p.into_args().contains(&serde_json::json!("a1")));

        let mut p = Params::new();
        let release = release_sql(&mut p, &KEY, "a1");
        assert!(release.contains("AND attempt_id = $4::uuid"), "{release}");
        assert!(release.contains("LEAST(blocked_until, NOW())"), "{release}");
        assert_eq!(p.into_args()[3], serde_json::json!("a1"));
    }

    #[test]
    fn a_claim_lasts_the_shorter_of_the_attempt_timeout_and_the_cooldown() {
        let mut p = Params::new();
        let sql = claim_sql(&mut p, &KEY, "a1", 600, 60_000);
        assert!(sql.contains("LEAST("), "{sql}");
        let args = p.into_args();
        assert!(args.contains(&serde_json::json!(600_000)));
        assert!(args.contains(&serde_json::json!(60_000)));
    }

    #[test]
    fn the_claim_re_checks_the_durable_clock_on_both_insert_and_update() {
        let sql = claim_sql(&mut Params::new(), &KEY, "a1", 60, 60_000);
        assert_eq!(
            sql.matches("NOT EXISTS (SELECT 1 FROM submission s")
                .count(),
            2
        );
        assert!(sql.contains("s.contest_id IS NULL"), "{sql}");
    }
}
