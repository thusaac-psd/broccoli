# submission-limit

Caps how many submissions a user may make to one problem, in one contest or
outside any contest. Configure `max_submissions` in the `limits` config at
problem, contest or contest-problem scope; `0` means unlimited.

## What counts

A slot is held by:

- every submission row for the user, problem and contest, whoever created it and
  whenever. That includes submissions made before the limit was enabled and rows
  an administrator created on the user's behalf. Deleting a submission frees its
  slot;
- every attempt in flight: one the `before_submission` hook admitted whose
  submission has not committed yet. It holds its slot until its submission
  commits (then the row counts instead) or until the attempt timeout the host
  publishes on the event passes, after which the host inserts nothing for it. An
  attempt rejected by a later hook or a failed insert therefore holds a slot for
  at most that timeout.

Each attempt is counted exactly once, and concurrent attempts cannot both take
the last slot: the check and the reservation are one SQL statement.

## Status endpoint

`GET /api/plugins/submission-limit/[contests/{contest_id}/]problems/{problem_id}/status`
returns `submissions_made`, the slots held by the rules above (in-flight
attempts included, and counted even when the limit is unlimited), together with
`max_submissions`, `remaining` and `unlimited`.
