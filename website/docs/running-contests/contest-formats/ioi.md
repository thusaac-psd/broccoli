---
title: IOI format
sidebar_label: IOI
sidebar_position: 2
---

# IOI format

This format builds each problem's score from subtasks and awards partial
credit. It can also hand out tokens and graded feedback.

The settings below live at two scopes. Contest level settings sit under
`[config.contest]` and are set once per contest. Task level settings sit
under `[config.task]` and are set per contest problem, so one problem can
carry different subtasks in different contests.

## Subtasks

A problem is divided into subtasks under `[config.task]`. Each subtask has a
name, a scoring method, a max score, and the test labels it covers.

| Key              | Default | Meaning                                    |
| ----------------- | ------- | ------------------------------------------- |
| `name`            | Subtask | label shown to contestants                  |
| `scoring_method`  | group_min | group_min, sum, or group_mul               |
| `max_score`       | 100     | points the subtask is worth in full          |
| `test_cases`      | empty   | labels of the test cases the subtask covers  |

A test case's label is the one set on the test case, or its numeric ID when
it has none. Validation, scoring and the scoreboard all read a subtask's
labels the same way, so they always agree on which test cases it covers. Saving
the config fails when a label names no test case, or names more than one (one
test case's label is another's ID). When a test data change forced through
(`force=true`) leaves such a label behind, that entry counts as a failed test
worth 0 points: it fails a `group_min` or `group_mul` subtask, and adds nothing
to a `sum` subtask, which the subtask's other tests score as if it were not
there. The problem's config issues list the entry until the config is fixed.

- `group_min`, the subtask scores its full max score only when every test in
  it passes, otherwise it scores zero. This is stricter than CMS GroupMin,
  which scales the subtask by its lowest test outcome, so a subtask with a
  half scoring test lands on zero here and on half under CMS.
- `sum`, the subtask gives partial credit in proportion to the tests that
  pass, weighted by each test's own point value.
- `group_mul`, the subtask multiplies the per test fractions together, so one
  weak test drags the whole subtask down.

A problem's total is the sum of its subtask scores. A problem with no
subtask config gets one `sum` subtask, shown as All Tests in each viewer's
language, covering every test case that is not a sample and is worth more than
zero points, with a max score equal to the sum of those points. It has no
stored name: the plugin's routes give it an empty `name`, which no configured
subtask can have.

A test case counts only toward the subtasks that name it, and the default
subtask names no sample. A case that counts toward no subtask scores 0 whatever
points it carries. A sample is still judged, for its feedback. Any other such
case is not judged at all, since it would score nothing and appears in no
subtask's feedback. A test case worth points that no subtask names, sample or
not, is listed on the problem's config issues, since those points count for
nothing.

A problem none of whose subtasks can score is not judged: every submission
to it ends as a system error that stands (`NOTHING_TO_SCORE`), as does every
submission to a problem with no test cases (`NO_TEST_CASES`). A subtask can
score when its max score is above zero and one of its labels names a test
case of the problem, one worth more than zero points for a `sum` subtask.
Either kind of problem is listed on its config issues and the contest's as
soon as it joins an IOI contest, whether or not a task config is saved. The
task config still saves meanwhile, and the issue clears once the test data
and the subtasks let the problem score.

Partial credit inside a subtask only appears when the checker scores a test
as a fraction, for example a testlib points checker. The built in
comparators are all or nothing.

## Scoring across submissions

`scoring_mode` is a contest setting that picks how a contestant's score for a
problem is chosen across all of their submissions, and it defaults to
`max_submission`.

- `max_submission`, the highest total score across all of a contestant's
  submissions.
- `sum_best_subtask`, the best score seen for each subtask across all
  submissions, then summed.
- `best_tokened_or_last`, the classic token rule, the higher of the best
  tokened submission and the last submission.

## Feedback

`feedback_level` is a contest setting that decides how much of a submission's
result a contestant sees before spending a token, and it defaults to `full`.

| Value             | Reveals                                                              |
| ------------------ | ---------------------------------------------------------------------- |
| `full`             | the verdict, the total score, and every test case's own verdict, score, time, and memory |
| `subtask_scores`   | the verdict, the total score, and the score of each subtask, with individual test case results held back |
| `total_only`       | the verdict and the total score only                                 |
| `none`             | nothing about the result: the submission reads as withheld, as a frozen one does |

:::note[Where the subtask breakdown shows up]

The extra detail at `subtask_scores` shows up in the task's subtask list, the
scoreboard's per problem scores, and the submission's subtask score endpoint.
The submission view itself hides per test case results at every level below
`full`.

:::

A submission shows the subtasks it was judged with and the score each one
got, and its total against the maximum it was judged with, so a change to a
problem's subtasks after judging changes no result a submission already
shows; judge it again to score it under the new subtasks. A result shows
no maximum until the one it was judged with is known. The `sum_best_subtask`
scoreboard combines every submission under the problem's current subtasks,
so its cells read against the current maximum; under the other scoring modes
a cell holds one submission's result and reads against the maximum that
result was judged with.

A subtask that cannot score is worth nothing: it adds nothing to the
problem's maximum and shows a maximum of 0, so a problem none of whose
subtasks can score has a maximum of 0 on the scoreboard. The scoreboard names
its columns by the contest's problem labels and counts only a submission's
standing result: a submission still being judged, or one whose system error
may still be judged again, leaves its cell at the last standing score.

The scoreboard follows the same level. At `none` it shows a contestant no
score at all, their own row included, so it lists names only, without ranks.
At `total_only` it shows totals without the per problem scores. Staff
(holders of `contest:manage` or `submission:view_all`) see every score, and
once the contest ends so does every contestant, on the scoreboard, in a
submission's subtask scores and in a problem's last submission status. A
token a contestant spent on a submission shows them that submission's scores
in both places at any level.

## Tokens

A token unlocks full feedback on one submission regardless of
`feedback_level`. A contestant spends a token on their own submission while
the contest is running, on a submission that has been judged, and a
submission can only be tokened once. The problem page's token panel offers a
token for the submission it shows when that one can take a token, or else for
the newest judged submission of that problem not yet tokened (so a submission
still judging never hides an older one), even when `feedback_level = "none"`
hides whether it was judged. Once spent, the submission's result shows
wherever the app shows the submission, in every open tab, with no reload.

`tokens.mode` picks the token model, and it defaults to `none`.

- `none`, tokens are off, feedback always follows `feedback_level`.
- `fixed_budget`, a contestant starts with a fixed number of tokens that
  never grows.
- `regenerating`, a contestant starts with a token budget that grows over
  time, up to a cap.

| Key                          | Default | Meaning                                 |
| ------------------------------ | ------- | ------------------------------------------ |
| `tokens.initial`               | 2       | tokens a contestant starts the contest with |
| `tokens.max`                   | 5       | cap for regenerating mode                   |
| `tokens.regen_interval_min`    | 30      | minutes between each regenerated token      |

`tokens.max` plays no part in `fixed_budget` mode, the budget stays at
`tokens.initial` for the whole contest.

## The scoreboard

`scoreboard_visibility` decides who sees the full board while the contest is
running, and it defaults to `admins_only`. Once the contest ends, every
contestant sees the full board no matter this setting, and staff (holders of
`contest:manage` or `submission:view_all`) always see it.

| Value                 | Meaning                                                        |
| ----------------------- | ----------------------------------------------------------------- |
| `admins_only`           | only staff see the full board while the contest runs, everyone else sees only their own row, without a rank |
| `all_contest_viewers`   | every contest viewer sees the full board while the contest runs |

Each problem carries a score time, the elapsed time from contest start
associated with reaching the contestant's score on that problem.
`scoreboard_tiebreaker` decides how tied total scores are ranked using those
per problem times, and it defaults to `max_score_time`.

| Value             | Meaning                                                           |
| ------------------ | -------------------------------------------------------------------- |
| `equal_rank`       | tied contestants share the same rank                                 |
| `sum_score_time`   | ties are broken by the sum of each problem's score time, faster total first |
| `max_score_time`   | ties are broken by the largest single problem's score time, faster first |
