//! The cooldown gate: its SQL and the one rule for how long a user must wait.
//!
//! Two clocks block a submission. The durable one runs from the user's last
//! committed submission for the (user, problem, contest) and lasts the full
//! cooldown. The claim clock closes the gap while a submission is in flight:
//! the hook passes before the server inserts the row, so concurrent attempts
//! would otherwise all see the old last submission. Each admission claims the
//! slot in `cooldown_claim`, which blocks others for [`claim_window`], long
//! enough for the winner's row to commit. An attempt rejected by a later hook
//! or a failed insert leaves only the claim, so it costs the user the claim
//! window rather than the whole cooldown. Every time is the database's `NOW()`.

use broccoli_server_sdk::prelude::Params;

/// The longest a claim blocks on its own.
pub const CLAIM_GRACE_SECS: u32 = 15;

pub const SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS cooldown_claim (\
     user_id       INTEGER NOT NULL, \
     problem_id    INTEGER NOT NULL, \
     contest_id    INTEGER NOT NULL DEFAULT 0, \
     last_claim_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
     PRIMARY KEY (user_id, problem_id, contest_id))";

/// How long a claim blocks: the grace, but never longer than the cooldown.
pub fn claim_window(cooldown: u32) -> u32 {
    cooldown.min(CLAIM_GRACE_SECS)
}

/// Seconds since each clock started, `None` when it never has.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Deserialize)]
pub struct Clocks {
    pub since_submission: Option<f64>,
    pub since_claim: Option<f64>,
}

impl Clocks {
    /// Whole seconds until a submission may pass, 0 when it may now.
    pub fn wait_secs(&self, cooldown: u32) -> u64 {
        let remaining = |window: u32, since: Option<f64>| {
            since
                .map_or(0.0, |s| f64::from(window) - s.max(0.0))
                .max(0.0)
        };
        remaining(cooldown, self.since_submission)
            .max(remaining(claim_window(cooldown), self.since_claim))
            .ceil() as u64
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

/// Claim the slot when neither clock blocks. Affects one row when the
/// submission may pass and none otherwise, in one statement: concurrent
/// claims serialize on the row and the `WHERE` re-checks against the claim a
/// concurrent winner committed.
pub fn claim_sql(p: &mut Params, key: &CooldownKey, cooldown: u32) -> String {
    let user = p.bind(key.user_id);
    let problem = p.bind(key.problem_id);
    let contest = p.bind(key.contest_id.unwrap_or(0));
    let recent = key.recent_submission(p, cooldown);
    let window = p.bind(claim_window(cooldown));
    format!(
        "INSERT INTO cooldown_claim (user_id, problem_id, contest_id, last_claim_at) \
         SELECT {user}, {problem}, {contest}, NOW() WHERE NOT {recent} \
         ON CONFLICT (user_id, problem_id, contest_id) DO UPDATE SET last_claim_at = NOW() \
         WHERE cooldown_claim.last_claim_at <= NOW() - ({window}::int * INTERVAL '1 second') \
           AND NOT {recent}"
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
           (SELECT EXTRACT(EPOCH FROM (NOW() - c.last_claim_at))::float8 \
              FROM cooldown_claim c WHERE {claim}) AS since_claim"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clocks(since_submission: Option<f64>, since_claim: Option<f64>) -> Clocks {
        Clocks {
            since_submission,
            since_claim,
        }
    }

    #[test]
    fn nothing_blocks_a_first_submission() {
        assert_eq!(clocks(None, None).wait_secs(60), 0);
    }

    #[test]
    fn the_durable_clock_runs_the_full_cooldown_from_the_last_submission() {
        assert_eq!(clocks(Some(20.0), Some(20.0)).wait_secs(60), 40);
        assert_eq!(clocks(Some(59.2), None).wait_secs(60), 1);
        assert_eq!(clocks(Some(60.0), None).wait_secs(60), 0);
    }

    #[test]
    fn a_claim_without_a_submission_blocks_only_for_the_claim_window() {
        assert_eq!(clocks(None, Some(5.0)).wait_secs(60), 10);
        assert_eq!(clocks(None, Some(15.0)).wait_secs(60), 0);
        // The window never outlasts a cooldown shorter than the grace.
        assert_eq!(clocks(None, Some(1.0)).wait_secs(4), 3);
    }

    #[test]
    fn the_longer_of_the_two_clocks_wins() {
        assert_eq!(clocks(Some(55.0), Some(2.0)).wait_secs(60), 13);
        assert_eq!(clocks(Some(30.0), Some(2.0)).wait_secs(60), 30);
    }

    #[test]
    fn the_claim_re_checks_the_durable_clock_on_both_insert_and_update() {
        let sql = claim_sql(
            &mut Params::new(),
            &CooldownKey {
                user_id: 1,
                problem_id: 2,
                contest_id: None,
            },
            60,
        );
        assert_eq!(
            sql.matches("NOT EXISTS (SELECT 1 FROM submission s")
                .count(),
            2
        );
        assert!(sql.contains("s.contest_id IS NULL"), "{sql}");
    }
}
