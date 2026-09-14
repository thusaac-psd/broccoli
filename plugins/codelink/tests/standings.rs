use codelink::config::ContestConfig;
use codelink::standings::*;

fn ac(id: i32, user: i32, problem: i32) -> Submission {
    Submission {
        submission_id: id,
        user_id: user,
        problem_id: problem,
        submitted_at_us: i64::from(id) * 1_000_000,
        accepted: true,
        pending: false,
    }
}

fn board(submissions: Vec<Submission>) -> Standings {
    board_with_config(submissions, &ContestConfig::default())
}

fn board_with_config(submissions: Vec<Submission>, config: &ContestConfig) -> Standings {
    calculate(
        (1..=20)
            .map(|user_id| Participant {
                user_id,
                username: format!("user{user_id}"),
            })
            .collect(),
        (1..=16)
            .map(|problem_id| Problem {
                problem_id,
                label: None,
            })
            .collect(),
        submissions,
        config,
    )
    .unwrap()
}

fn row(board: &Standings, user_id: i32) -> &Standing {
    board.rows.iter().find(|r| r.user_id == user_id).unwrap()
}

#[test]
fn only_first_two_distinct_contestants_receive_credit() {
    let result = board(vec![ac(1, 1, 1), ac(2, 1, 1), ac(3, 2, 1), ac(4, 3, 1)]);
    assert_eq!(row(&result, 1).credited, 1);
    assert_eq!(row(&result, 1).accepted, 1);
    assert_eq!(row(&result, 2).credited, 1);
    assert_eq!(row(&result, 3).credited, 0);
    assert_eq!(row(&result, 3).problems[&1].status, CreditStatus::SlotsFull);
    assert_eq!(result.problems[0].remaining, 0);
    assert_eq!(result.problems[0].awards[1].user_id, 2);
}

#[test]
fn qualification_requires_two_credited_distinct_problems() {
    let result = board(vec![ac(1, 1, 1), ac(2, 2, 1), ac(3, 3, 1), ac(4, 3, 2)]);
    let contestant = row(&result, 3);
    assert_eq!(contestant.accepted, 2);
    assert_eq!(contestant.credited, 1);
    assert!(!contestant.qualified);
    assert_eq!(contestant.qualified_at_seconds, None);
}

#[test]
fn qualified_contestants_keep_their_original_slots_but_take_no_more() {
    let result = board(vec![
        ac(1, 1, 1),
        ac(2, 1, 2),
        ac(3, 1, 3),
        ac(4, 1, 4),
        ac(5, 2, 3),
        ac(6, 3, 3),
        ac(7, 4, 3),
        ac(8, 2, 1),
        ac(9, 3, 1),
    ]);
    let first = row(&result, 1);
    assert!(first.qualified && first.qualification_confirmed);
    assert_eq!(first.credited, 2);
    assert_eq!(first.qualified_at_seconds, Some(2));
    assert_eq!(first.accepted, 4);
    assert_eq!(first.problems[&3].status, CreditStatus::AfterQualification);
    assert_eq!(
        result.problems[2]
            .awards
            .iter()
            .map(|a| a.user_id)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    assert_eq!(result.problems[3].remaining, 2);
    assert_eq!(row(&result, 4).problems[&3].status, CreditStatus::SlotsFull);
    assert_eq!(row(&result, 3).problems[&1].status, CreditStatus::SlotsFull);
    assert!(row(&result, 2).qualified);
}

#[test]
fn submission_time_and_id_determine_order_regardless_of_judge_delivery() {
    let mut first = ac(10, 1, 1);
    first.submitted_at_us = 1_000_002;
    let mut second = ac(9, 2, 1);
    second.submitted_at_us = 1_000_003;
    let mut third = ac(11, 3, 1);
    third.submitted_at_us = second.submitted_at_us;
    let result = board(vec![third, second, first]);
    assert_eq!(
        result.problems[0]
            .awards
            .iter()
            .map(|a| a.user_id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(row(&result, 3).credited, 0);
}

#[test]
fn failures_and_outside_participants_or_problems_do_not_take_slots() {
    let mut failure = ac(1, 1, 1);
    failure.accepted = false;
    let result = board(vec![
        failure,
        ac(2, 99, 1),
        ac(3, 2, 99),
        ac(4, 2, 1),
        ac(5, 1, 1),
    ]);
    assert_eq!(
        result.problems[0]
            .awards
            .iter()
            .map(|a| a.user_id)
            .collect::<Vec<_>>(),
        [2, 1]
    );
    assert_eq!(row(&result, 2).credited, 1);
}

#[test]
fn earlier_pending_submission_marks_qualification_provisional_until_resolved() {
    let mut pending = ac(1, 1, 1);
    pending.accepted = false;
    pending.pending = true;
    let submissions = vec![pending, ac(2, 2, 1), ac(3, 3, 1), ac(4, 3, 2)];
    let provisional = board(submissions.clone());
    assert!(row(&provisional, 3).qualified);
    assert!(!row(&provisional, 3).qualification_confirmed);
    assert_eq!(provisional.pending_submissions, 1);

    let mut resolved = submissions;
    resolved[0].pending = false;
    let failed = board(resolved.clone());
    assert!(row(&failed, 3).qualification_confirmed);
    assert_eq!(failed.pending_submissions, 0);

    resolved[0].accepted = true;
    let accepted = board(resolved);
    assert!(!row(&accepted, 3).qualified);
    assert_eq!(row(&accepted, 3).credited, 1);
}

#[test]
fn later_pending_or_accepted_submissions_do_not_change_confirmed_qualification() {
    let mut later = ac(3, 2, 1);
    later.accepted = false;
    later.pending = true;
    let first = board(vec![ac(1, 1, 1), ac(2, 1, 2), later]);
    assert!(row(&first, 1).qualification_confirmed);
    let after = board(vec![ac(1, 1, 1), ac(2, 1, 2), ac(3, 2, 1), ac(4, 1, 3)]);
    assert_eq!(first.rows[0].user_id, after.rows[0].user_id);
    assert_eq!(
        first.rows[0].qualified_at_seconds,
        after.rows[0].qualified_at_seconds
    );
    assert_eq!(after.problems[2].remaining, 2);
}

#[test]
fn replay_does_not_keep_obsolete_credits_after_an_applied_rejudge() {
    let mut submissions = vec![ac(1, 1, 1), ac(2, 1, 2), ac(3, 1, 3), ac(4, 2, 3)];
    assert!(row(&board(submissions.clone()), 1).qualified);
    submissions[0].accepted = false;
    let result = board(submissions);
    assert_eq!(result.problems[0].remaining, 2);
    assert_eq!(row(&result, 1).problems[&3].slot, Some(1));
    assert_eq!(row(&result, 2).problems[&3].slot, Some(2));
    assert_eq!(row(&result, 1).qualified_at_seconds, Some(3));
}

#[test]
fn sixteen_problems_provide_at_most_sixteen_qualifiers() {
    let mut submissions = Vec::new();
    for problem in 1..=16 {
        for user in 1..=20 {
            submissions.push(ac(submissions.len() as i32 + 1, user, problem));
        }
    }
    let result = board(submissions);
    assert_eq!(result.qualified_count, 16);
    assert_eq!(result.confirmed_qualified_count, 16);
    assert_eq!(result.rows.iter().map(|r| r.credited).sum::<usize>(), 32);
    assert!(result.rows.iter().all(|r| r.credited <= 2));
    assert!(result.problems.iter().all(|p| p.awards.len() == 2));
    assert_eq!(result.problems[15].label, "P");
}

#[test]
fn problem_ids_keep_duplicate_labels_independent_and_empty_contest_is_valid() {
    let result = calculate(
        vec![Participant {
            user_id: 1,
            username: "one".into(),
        }],
        vec![
            Problem {
                problem_id: 1,
                label: Some("same".into()),
            },
            Problem {
                problem_id: 2,
                label: Some("same".into()),
            },
        ],
        vec![ac(1, 1, 1), ac(2, 1, 2)],
        &ContestConfig::default(),
    )
    .unwrap();
    assert!(result.rows[0].qualified);
    assert_eq!(result.rows[0].problems.len(), 2);
    let empty = calculate(vec![], vec![], vec![], &ContestConfig::default()).unwrap();
    assert_eq!(empty.qualified_count, 0);
    assert!(empty.rows.is_empty());
}

#[test]
fn custom_slots_and_qualification_threshold_control_allocation() {
    let config = ContestConfig {
        expected_problem_count: 0,
        slots_per_problem: 3,
        solves_to_qualify: 3,
        scoreboard_refresh_seconds: 12,
    };
    let submissions = vec![
        ac(1, 1, 1),
        ac(2, 2, 1),
        ac(3, 3, 1),
        ac(4, 4, 1),
        ac(5, 1, 2),
    ];
    let before = board_with_config(submissions.clone(), &config);
    assert_eq!(row(&before, 3).credited, 1);
    assert_eq!(row(&before, 4).credited, 0);
    assert!(!row(&before, 1).qualified);

    let after = board_with_config(
        [submissions, vec![ac(6, 1, 3), ac(7, 1, 4), ac(8, 4, 4)]].concat(),
        &config,
    );
    assert!(row(&after, 1).qualified);
    assert_eq!(row(&after, 1).credited, 3);
    assert_eq!(row(&after, 1).qualified_at_seconds, Some(6));
    assert_eq!(
        row(&after, 1).problems[&4].status,
        CreditStatus::AfterQualification
    );
    assert_eq!(after.problems[3].awards[0].user_id, 4);
    assert_eq!(after.problems[3].remaining, 2);
    assert_eq!(after.problem_count, 16);
    assert_eq!(after.config, config);
}

#[test]
fn one_slot_and_one_credit_qualifies_immediately_without_taking_later_slots() {
    let config = ContestConfig {
        slots_per_problem: 1,
        solves_to_qualify: 1,
        ..Default::default()
    };
    let result = board_with_config(
        vec![ac(1, 1, 1), ac(2, 2, 1), ac(3, 1, 2), ac(4, 2, 2)],
        &config,
    );
    assert_eq!(result.confirmed_qualified_count, 2);
    assert_eq!(row(&result, 1).qualified_at_seconds, Some(1));
    assert_eq!(row(&result, 2).qualified_at_seconds, Some(4));
    assert_eq!(result.problems[1].awards[0].user_id, 2);
}
