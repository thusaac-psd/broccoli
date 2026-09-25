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

fn pending(id: i32, user: i32, problem: i32) -> Submission {
    Submission {
        accepted: false,
        pending: true,
        ..ac(id, user, problem)
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
    assert_eq!(contestant.qualification_verdict, None);
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
    assert_eq!(
        first.qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
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
    assert_eq!(
        row(&result, 2).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
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
fn earlier_pending_submission_requires_a_verdict_before_counting_qualification() {
    let mut pending = ac(1, 1, 1);
    pending.accepted = false;
    pending.pending = true;
    let submissions = vec![pending, ac(2, 2, 1), ac(3, 3, 1), ac(4, 3, 2)];
    let waiting = board(submissions.clone());
    assert_eq!(
        row(&waiting, 3).qualification_verdict,
        Some(QualificationVerdict::Pending)
    );
    assert_eq!(row(&waiting, 3).credited, 2);
    assert_eq!(row(&waiting, 3).qualified_at_seconds, None);
    assert_eq!(waiting.qualified_count, 0);
    assert_eq!(waiting.pending_submissions, 1);

    let mut resolved = submissions;
    resolved[0].pending = false;
    let failed = board(resolved.clone());
    assert_eq!(
        row(&failed, 3).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
    assert_eq!(failed.pending_submissions, 0);
    assert_eq!(failed.qualified_count, 1);
    assert_eq!(row(&failed, 3).qualified_at_seconds, Some(4));

    resolved[0].accepted = true;
    let accepted = board(resolved);
    assert_eq!(row(&accepted, 3).qualification_verdict, None);
    assert_eq!(row(&accepted, 3).credited, 1);
    assert_eq!(accepted.qualified_count, 0);
}

#[test]
fn own_pending_second_problem_waits_for_a_verdict_without_claiming_qualification() {
    let submissions = vec![ac(1, 1, 1), pending(2, 1, 2)];
    let waiting = board(submissions.clone());
    assert_eq!(
        row(&waiting, 1).qualification_verdict,
        Some(QualificationVerdict::Pending)
    );
    assert_eq!(row(&waiting, 1).credited, 1);
    assert_eq!(row(&waiting, 1).qualified_at_seconds, None);
    assert_eq!(waiting.qualified_count, 0);

    let mut resolved = submissions;
    resolved[1].pending = false;
    let failed = board(resolved.clone());
    assert_eq!(row(&failed, 1).qualification_verdict, None);
    assert_eq!(row(&failed, 1).qualified_at_seconds, None);

    resolved[1].accepted = true;
    let accepted = board(resolved);
    assert_eq!(
        row(&accepted, 1).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
    assert_eq!(row(&accepted, 1).qualified_at_seconds, Some(2));
    assert_eq!(accepted.qualified_count, 1);
}

#[test]
fn insufficient_distinct_possible_credits_do_not_receive_a_verdict() {
    let result = board(vec![
        pending(1, 1, 1),
        pending(2, 1, 1),
        ac(3, 2, 2),
        ac(4, 3, 2),
        ac(5, 4, 3),
        pending(6, 4, 2),
    ]);
    assert_eq!(result.qualified_count, 0);
    // Repeating a pending problem cannot meet the two-problem threshold.
    assert_eq!(row(&result, 1).qualification_verdict, None);
    // Pending work on a certainly full problem cannot supply a second credit.
    assert_eq!(row(&result, 4).qualification_verdict, None);
    // An unrelated pending submission does not give inactive contestants a verdict.
    assert_eq!(row(&result, 5).qualification_verdict, None);
}

#[test]
fn later_pending_or_accepted_submissions_do_not_change_confirmed_qualification() {
    let mut later = ac(3, 2, 1);
    later.accepted = false;
    later.pending = true;
    let first = board(vec![ac(1, 1, 1), ac(2, 1, 2), later]);
    assert_eq!(
        row(&first, 1).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
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
    assert_eq!(
        row(&board(submissions.clone()), 1).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
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
    assert_eq!(
        result.rows[0].qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
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
    assert_eq!(row(&before, 1).qualification_verdict, None);

    let after = board_with_config(
        [submissions, vec![ac(6, 1, 3), ac(7, 1, 4), ac(8, 4, 4)]].concat(),
        &config,
    );
    assert_eq!(
        row(&after, 1).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
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
    assert_eq!(result.qualified_count, 2);
    assert_eq!(row(&result, 1).qualified_at_seconds, Some(1));
    assert_eq!(row(&result, 2).qualified_at_seconds, Some(4));
    assert_eq!(result.problems[1].awards[0].user_id, 2);
}

#[test]
fn pending_on_a_full_problem_does_not_delay_unrelated_qualification() {
    let result = board(vec![
        ac(1, 1, 1),
        ac(2, 2, 1),
        pending(3, 3, 1),
        ac(4, 1, 2),
    ]);
    assert_eq!(result.pending_submissions, 1);
    assert_eq!(
        row(&result, 1).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
}

#[test]
fn pending_on_an_unrelated_problem_does_not_delay_qualification() {
    let result = board(vec![pending(1, 3, 3), ac(2, 1, 1), ac(3, 1, 2)]);
    assert_eq!(result.pending_submissions, 1);
    assert_eq!(
        row(&result, 1).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
}

#[test]
fn duplicate_pending_submission_cannot_take_another_slot() {
    let result = board(vec![ac(1, 1, 1), pending(2, 1, 1), ac(3, 1, 2)]);
    assert_eq!(result.pending_submissions, 1);
    assert_eq!(result.problems[0].remaining, 1);
    assert_eq!(
        row(&result, 1).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
}

#[test]
fn pending_from_a_confirmed_qualifier_does_not_block_other_contestants() {
    let result = board(vec![
        ac(1, 1, 1),
        ac(2, 1, 2),
        pending(3, 1, 3),
        ac(4, 2, 3),
        ac(5, 2, 4),
    ]);
    assert_eq!(result.qualified_count, 2);
    assert_eq!(result.pending_submissions, 1);
}

#[test]
fn pending_rival_does_not_block_a_slot_that_is_available_in_every_outcome() {
    let result = board(vec![pending(1, 2, 1), ac(2, 1, 1), ac(3, 1, 2)]);
    assert_eq!(
        row(&result, 1).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
}

#[test]
fn own_pending_solve_does_not_block_guaranteed_qualification() {
    let result = board(vec![pending(1, 1, 3), ac(2, 1, 1), ac(3, 1, 2)]);
    assert_eq!(
        row(&result, 1).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
}

#[test]
fn repeated_pending_attempts_reserve_only_one_possible_place_per_contestant() {
    let result = board(vec![
        pending(1, 2, 1),
        pending(2, 2, 1),
        ac(3, 1, 1),
        ac(4, 1, 2),
    ]);
    assert_eq!(result.pending_submissions, 2);
    assert_eq!(
        row(&result, 1).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
}

#[test]
fn additional_known_solves_can_guarantee_eligibility_despite_a_disputed_slot() {
    let mut submissions = vec![pending(1, 2, 1), pending(2, 3, 1), ac(3, 1, 1), ac(4, 1, 2)];
    assert_ne!(
        row(&board(submissions.clone()), 1).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
    submissions.push(ac(5, 1, 3));
    assert_eq!(
        row(&board(submissions), 1).qualification_verdict,
        Some(QualificationVerdict::Qualified)
    );
}

#[test]
fn uncertainty_propagates_through_another_contestants_qualification() {
    let submissions = vec![
        pending(1, 1, 1),
        ac(2, 2, 1),
        ac(3, 3, 1),
        ac(4, 3, 2),
        ac(5, 3, 3),
        ac(6, 4, 3),
        ac(7, 5, 3),
        ac(8, 5, 4),
    ];
    let waiting = board(submissions.clone());
    assert_eq!(
        row(&waiting, 5).qualification_verdict,
        Some(QualificationVerdict::Pending)
    );
    assert_eq!(row(&waiting, 5).credited, 2);
    assert_eq!(row(&waiting, 5).qualified_at_seconds, None);

    let mut resolved = submissions;
    resolved[0].pending = false;
    resolved[0].accepted = true;
    let final_board = board(resolved);
    assert_eq!(
        row(&final_board, 3).problems[&3].status,
        CreditStatus::Credited
    );
    assert_eq!(row(&final_board, 5).qualification_verdict, None);
}

#[test]
fn confirmation_is_sound_for_every_pending_outcome_in_generated_small_histories() {
    // Enumerate every AC/failure resolution of each generated pending set. This
    // checks the guarantee against completed scoreboards, including interactions
    // across problems and qualification thresholds, without duplicating replay.
    let mut seed = 0x5eed_u64;
    for history in 0..512 {
        let config = ContestConfig {
            slots_per_problem: 1 + history % 3,
            solves_to_qualify: 1 + (history / 3) % 3,
            ..Default::default()
        };
        let mut submissions = Vec::new();
        let mut pending_indices = Vec::new();
        for id in 1..=12 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let user = 1 + ((seed >> 32) % 4) as i32;
            let problem = 1 + ((seed >> 40) % 4) as i32;
            let outcome = (seed >> 48) % 4;
            let mut submission = ac(id, user, problem);
            if outcome == 0 && pending_indices.len() < 4 {
                submission.accepted = false;
                submission.pending = true;
                pending_indices.push(submissions.len());
            } else if outcome == 1 {
                submission.accepted = false;
            }
            submissions.push(submission);
        }
        let initial = board_with_config(submissions.clone(), &config);
        for mask in 0..(1 << pending_indices.len()) {
            let mut resolved = submissions.clone();
            for (bit, &index) in pending_indices.iter().enumerate() {
                resolved[index].pending = false;
                resolved[index].accepted = mask & (1 << bit) != 0;
            }
            let final_board = board_with_config(resolved, &config);
            assert!(final_board.rows.iter().all(|r| {
                r.qualification_verdict != Some(QualificationVerdict::Pending)
                    && (r.qualification_verdict == Some(QualificationVerdict::Qualified))
                        == (r.credited == config.solves_to_qualify)
            }));
            for contestant in initial
                .rows
                .iter()
                .filter(|r| r.qualification_verdict.is_none())
            {
                assert_ne!(
                    row(&final_board, contestant.user_id).qualification_verdict,
                    Some(QualificationVerdict::Qualified),
                    "history {history}, mask {mask}, user {} should have awaited judging",
                    contestant.user_id
                );
            }
            for contestant in initial
                .rows
                .iter()
                .filter(|r| r.qualification_verdict == Some(QualificationVerdict::Qualified))
            {
                assert!(
                    row(&final_board, contestant.user_id).qualification_verdict
                        == Some(QualificationVerdict::Qualified),
                    "history {history}, mask {mask}, user {} was incorrectly confirmed",
                    contestant.user_id
                );
            }
            assert!(
                final_board
                    .problems
                    .iter()
                    .all(|p| p.awards.len() <= config.slots_per_problem)
            );
            assert!(
                final_board
                    .rows
                    .iter()
                    .all(|r| r.credited <= config.solves_to_qualify)
            );
        }
    }
}
