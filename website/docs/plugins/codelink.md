---
title: Codelink
sidebar_label: Codelink
sidebar_position: 2
---

# Codelink

Use the `codelink` contest type for the morning qualification round. Contestants
choose freely among the contest's problems and qualify for the afternoon by
earning scoring slots. Each contest can set its own slot limit, qualification
threshold, and refresh interval.

## Build and enable the plugin

Run these commands from the Broccoli repository after installing its development
dependencies.

```bash
pnpm --filter @broccoli/web-sdk build
just build-plugin plugins/codelink --install
```

The build produces `plugins/codelink/codelink.wasm` and the frontend bundle under
`plugins/codelink/web/dist`. The server discovers the plugin from its configured
plugins directory, which defaults to `./plugins`. Start the server or use
**Reload all plugins** in the admin area to discover it.

In the contest editor, select `codelink` as the contest type, set the morning
start and end times, and add the problems. Each problem needs test cases and
its usual evaluator, checker, and language plugins. Enroll the contestants in
the morning contest. The standings include enrolled contestants only.

## Configure a contest

Open the contest's **Configure** dialog in the admin area and select the
`codelink` plugin's `contest` settings.

| Setting | Default | Effect |
| --- | --- | --- |
| **Expected Problem Count** (`expected_problem_count`) | 16 | Show a notice when the contest has a different number of problems. Set to 0 to disable the check. |
| **Scoring Slots per Problem** (`slots_per_problem`) | 2 | Maximum number of different eligible contestants who can earn credit on each problem. |
| **Credited Problems to Qualify** (`solves_to_qualify`) | 2 | Number of different problems on which a contestant must earn a slot to qualify. |
| **Scoreboard Refresh Interval** (`scoreboard_refresh_seconds`) | 5 | Automatic refresh interval in seconds. Set to 0 for manual refresh only. |

Problem counts and scoring limits accept integers up to 1000. Both scoring
limits must be at least 1. The refresh interval accepts integers from 0 to 3600.
The expected count only controls the setup notice. The actual problem list and
the problem count in the rules always come from the contest.

The defaults match the morning round with 16 problems, two slots per problem,
and two credited problems to qualify. Set these values before the round starts.
Saving a change updates the rules and recalculates existing standings on the
next refresh. Settings apply to this contest only. No rebuild is needed for
configuration changes after the updated plugin has been loaded.

## Award scoring slots

Open the contest's **Rankings** page to see the configured slots available on
each problem. The **Overview** page displays the same configured rules.

1. Order accepted submissions by their submission time. The smaller submission
   ID comes first when timestamps are identical. Evaluation queue order does
   not decide who receives a slot.
2. Count only the first accepted submission from each contestant on each problem.
3. Award an available slot if the contestant has not qualified yet, up to the
   configured slot limit for each problem.
4. Qualify a contestant when their credited problem count reaches the configured
   threshold. Keep
   those slots occupied. Later accepted submissions remain visible but take no
   additional slots and do not change that contestant's qualification time.

An accepted solution without a slot does not count toward qualification. A
contestant who has already qualified is skipped when allocating slots on later
problems, so the next eligible contestant can receive one.

With the default slot limit and qualification threshold, the following events
produce these results.

| Event | Outcome |
| --- | --- |
| Alice earns slots on A and B | Alice qualifies and retains both slots |
| Alice later passes C | The AC is visible and both C slots remain available |
| Bob and Carol then pass C | Each receives one C slot |
| Dave also passes C | The AC is visible without a scoring slot |

Only submissions made within the morning contest window count. A submission
made before the deadline can still earn a slot if evaluation finishes after the
deadline. Practice submissions and submissions outside the contest window do
not occupy slots. Failed evaluations and problems without test cases do not
grant an AC.

## Read qualification status

The board uses the configured refresh interval, including after the contest
ends while queued evaluations finish. Use **Auto refresh** to pause it or
**Refresh** to request an update. When the interval is 0, the page displays
**Manual refresh only**.

| Status | Meaning |
| --- | --- |
| Qualified | Qualification is guaranteed however the remaining evaluations finish |
| Awaiting judging | Unfinished evaluations prevent confirming whether the contestant qualifies |

The board shows these two verdicts only. Contestants without enough possible
credited problems have no verdict and can follow their progress in **Credited**.
The qualifier count and qualification times include contestants marked **Qualified** only.

Pending submissions include work still in the queue before a judgement has been
created. A contestant can await a verdict while their own submission finishes or
while an earlier submission could change their slots. Attempts on a problem whose slots are definitely full, duplicate attempts
on an already accepted problem, and attempts by confirmed qualifiers do not
delay other contestants' qualification.

The board confirms eligibility when it can guarantee qualification regardless
of the pending results. It also accounts for those results changing when other
contestants qualify and stop taking further slots. The displayed slots and times
reflect the current results and can still change while pending work finishes,
even when eligibility is already confirmed.

Applied rejudges and rule changes can recalculate qualification. A rejudge that
has not been applied does not replace the official result. Finish pending
evaluations and review any rejudges before using the list for the afternoon round.

The score shown on an individual submission describes whether its solution
passed. Use **Qualified** on the Codelink board to determine qualification. The
plugin records qualification on this board and does not enroll contestants in a
separate afternoon contest.
