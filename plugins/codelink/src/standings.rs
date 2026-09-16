use std::collections::{BTreeMap, HashMap, HashSet};

use broccoli_server_sdk::error::SdkError;
use serde::{Deserialize, Serialize};

use crate::config::ContestConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct Participant {
    pub user_id: i32,
    pub username: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Problem {
    pub problem_id: i32,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Submission {
    pub submission_id: i32,
    pub user_id: i32,
    pub problem_id: i32,
    /// Microseconds from contest start to submission, independent of judge time.
    pub submitted_at_us: i64,
    pub accepted: bool,
    pub pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CreditStatus {
    Credited,
    SlotsFull,
    AfterQualification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationVerdict {
    /// Unfinished evaluations prevent confirming whether the contestant qualifies.
    Pending,
    /// Eligibility survives every resolution of the visible pending submissions.
    Qualified,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProblemCell {
    pub status: CreditStatus,
    pub submission_id: i32,
    pub time_seconds: i64,
    pub slot: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Standing {
    pub user_id: i32,
    pub username: String,
    pub credited: usize,
    pub accepted: usize,
    /// None means there are not enough possible credited problems to qualify.
    pub qualification_verdict: Option<QualificationVerdict>,
    /// Only exposed for confirmed qualifiers; the exact time may still change.
    pub qualified_at_seconds: Option<i64>,
    pub problems: BTreeMap<i32, ProblemCell>,
    #[serde(skip)]
    last_credit: Option<(i64, i32)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SlotAward {
    pub user_id: i32,
    pub username: String,
    pub submission_id: i32,
    pub time_seconds: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProblemSlots {
    pub problem_id: i32,
    pub label: String,
    pub remaining: usize,
    /// Position in this array is the awarded slot, starting at one.
    pub awards: Vec<SlotAward>,
}

#[derive(Debug, Serialize)]
pub struct Standings {
    #[serde(flatten)]
    pub config: ContestConfig,
    pub problem_count: usize,
    /// Counts confirmed qualifiers only.
    pub qualified_count: usize,
    pub pending_submissions: usize,
    pub problems: Vec<ProblemSlots>,
    pub rows: Vec<Standing>,
}

/// Conservative bounds over all possible AC/failure outcomes of pending work.
/// Possible owners reserve capacity until their results are known. A known AC
/// with room even under those reservations proves "owns this slot OR already
/// qualified". Proofs on enough distinct problems therefore guarantee eligibility,
/// even when its exact credited problems or qualification time can still change.
struct Confirmation {
    possible_owners: Vec<HashSet<usize>>,
    certain_owners: Vec<HashSet<usize>>,
    possible_problems: Vec<HashSet<usize>>,
    proofs: Vec<HashSet<usize>>,
}

impl Confirmation {
    fn new(users: usize, problems: usize) -> Self {
        Self {
            possible_owners: vec![HashSet::new(); problems],
            certain_owners: vec![HashSet::new(); problems],
            possible_problems: vec![HashSet::new(); users],
            proofs: vec![HashSet::new(); users],
        }
    }

    fn is_confirmed(&self, user: usize, config: &ContestConfig) -> bool {
        self.proofs[user].len() >= config.solves_to_qualify
    }

    fn verdict(&self, user: usize, config: &ContestConfig) -> Option<QualificationVerdict> {
        if self.is_confirmed(user, config) {
            Some(QualificationVerdict::Qualified)
        } else if self.possible_problems[user].len() >= config.solves_to_qualify {
            Some(QualificationVerdict::Pending)
        } else {
            None
        }
    }

    fn observe(&mut self, user: usize, problem: usize, accepted: bool, config: &ContestConfig) {
        if self.is_confirmed(user, config)
            || self.certain_owners[problem].len() >= config.slots_per_problem
        {
            return;
        }

        let other_owners = self.possible_owners[problem].len()
            - usize::from(self.possible_owners[problem].contains(&user));
        if accepted && other_owners < config.slots_per_problem {
            self.proofs[user].insert(problem);
            let other_problems = self.possible_problems[user].len()
                - usize::from(self.possible_problems[user].contains(&problem));
            if other_problems < config.solves_to_qualify {
                // Without this problem the contestant cannot already have
                // qualified, so its place really is occupied in every outcome.
                self.certain_owners[problem].insert(user);
            }
        }

        // Even a contestant at the displayed credit limit may need this place
        // if an earlier pending rival displaces one of their current credits.
        // Keep that possibility so uncertainty can propagate through other problems.
        self.possible_owners[problem].insert(user);
        self.possible_problems[user].insert(problem);
    }
}

/// Replay one snapshot of official results in submission order. All users and
/// problems must be included before any display filtering: their quotas interact.
/// No per-submission counters are persisted, so retries cannot consume extra slots
/// and an applied rejudge is reflected by replaying the new official results.
pub fn calculate(
    participants: Vec<Participant>,
    problems: Vec<Problem>,
    mut submissions: Vec<Submission>,
    config: &ContestConfig,
) -> Result<Standings, SdkError> {
    config.validate()?;
    let mut rows: Vec<_> = participants
        .into_iter()
        .map(|p| Standing {
            user_id: p.user_id,
            username: p.username,
            credited: 0,
            accepted: 0,
            qualification_verdict: None,
            qualified_at_seconds: None,
            problems: BTreeMap::new(),
            last_credit: None,
        })
        .collect();
    let user_indices: HashMap<_, _> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.user_id, i))
        .collect();
    let mut problems: Vec<_> = problems
        .into_iter()
        .enumerate()
        .map(|(i, p)| ProblemSlots {
            problem_id: p.problem_id,
            label: p
                .label
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| {
                    // Use a numeric fallback after the alphabet is exhausted.
                    if i < 26 {
                        ((b'A' + i as u8) as char).to_string()
                    } else {
                        (i + 1).to_string()
                    }
                }),
            remaining: config.slots_per_problem,
            awards: Vec::new(),
        })
        .collect();
    let problem_indices: HashMap<_, _> = problems
        .iter()
        .enumerate()
        .map(|(i, p)| (p.problem_id, i))
        .collect();

    // Preserve microsecond precision. Submission id breaks exact timestamp ties
    // deterministically, independent of SQL row order or worker delivery order.
    submissions.sort_by_key(|s| (s.submitted_at_us, s.submission_id));
    let mut confirmation = Confirmation::new(rows.len(), problems.len());
    let mut pending_submissions = 0;
    for submission in submissions {
        let (Some(&user_index), Some(&problem_index)) = (
            user_indices.get(&submission.user_id),
            problem_indices.get(&submission.problem_id),
        ) else {
            continue;
        };
        if submission.pending {
            pending_submissions += 1;
        }
        if !submission.accepted && !submission.pending {
            continue;
        }
        let row = &mut rows[user_index];
        // Only the first AC on each distinct problem matters for a contestant.
        if row.problems.contains_key(&submission.problem_id) {
            continue;
        }
        confirmation.observe(
            user_index,
            problem_index,
            submission.accepted && !submission.pending,
            config,
        );
        if submission.pending {
            continue;
        }
        let problem = &mut problems[problem_index];
        let time_seconds = submission.submitted_at_us.div_euclid(1_000_000);
        let status = if row.credited >= config.solves_to_qualify {
            CreditStatus::AfterQualification
        } else if problem.remaining == 0 {
            CreditStatus::SlotsFull
        } else {
            CreditStatus::Credited
        };
        let slot = if status == CreditStatus::Credited {
            problem.awards.push(SlotAward {
                user_id: row.user_id,
                username: row.username.clone(),
                submission_id: submission.submission_id,
                time_seconds,
            });
            problem.remaining -= 1;
            row.credited += 1;
            row.last_credit = Some((submission.submitted_at_us, submission.submission_id));
            if row.credited == config.solves_to_qualify {
                row.qualified_at_seconds = Some(time_seconds);
            }
            Some(problem.awards.len())
        } else {
            None
        };
        row.accepted += 1;
        row.problems.insert(
            submission.problem_id,
            ProblemCell {
                status,
                submission_id: submission.submission_id,
                time_seconds,
                slot,
            },
        );
    }

    for (index, row) in rows.iter_mut().enumerate() {
        row.qualification_verdict = confirmation.verdict(index, config);
        if row.qualification_verdict != Some(QualificationVerdict::Qualified) {
            row.qualified_at_seconds = None;
        }
    }

    // Order by displayed credit progress. Extra ACs after the credit limit do
    // not change this ordering or the time at which the threshold was reached.
    rows.sort_by_key(|r| (std::cmp::Reverse(r.credited), r.last_credit, r.user_id));
    Ok(Standings {
        config: config.clone(),
        problem_count: problems.len(),
        qualified_count: rows
            .iter()
            .filter(|r| r.qualification_verdict == Some(QualificationVerdict::Qualified))
            .count(),
        pending_submissions,
        problems,
        rows,
    })
}
