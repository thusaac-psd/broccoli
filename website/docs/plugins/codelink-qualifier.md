---
title: Codelink qualifier
sidebar_label: Codelink qualifier
sidebar_position: 2
---

# Codelink qualifier

The `codelink-qualifier` contest type runs the Codelink morning round.
Contestants pick any problems they like, and each problem gives scoring slots
to the first few eligible contestants who solve it. Each slot earns the
problem's points. Reach 3 points to qualify directly for the afternoon
[Codelink bracket](./codelink-bracket.md). After the round, contestants with
2 points, then 1 point, fill the remaining places up to 16.

## Enable the plugins

The Codelink qualifier and bracket plugins ship with every release bundle but
stay off until you enable them. On each server node, run this from the bundle
directory, then select **Reload All** on the admin **Plugins** page.

```bash
./enable-plugin.sh codelink-qualifier codelink-bracket
```

To enable them while installing instead, set `BROCCOLI_PLUGINS` for the
installer. This works the same way for `install.sh` and `install-native.sh`.

```bash
BROCCOLI_PLUGINS="codelink-qualifier codelink-bracket" ./install.sh server
```

In a development checkout, build and install them from the repository.

```bash
pnpm --filter @broccoli/web-sdk build
just build-plugin plugins/codelink-qualifier --install
just build-plugin plugins/codelink-bracket --install
```

## Create the contest

1. In the contest editor, choose `codelink-qualifier` as the contest type and
   set the start and end of the morning round.
2. Add the problems. Each one needs test cases and its usual evaluator,
   checker, and language plugins.
3. Enroll every contestant. The standings list enrolled contestants only.

## Configure the rules

Open the contest's **Configure** dialog in the admin area and choose the
`codelink-qualifier` plugin's `contest` settings.

| Setting | Default | Effect |
| --- | --- | --- |
| **Expected Problem Count** (`expected_problem_count`) | 20 | Shows a notice when the contest has a different number of problems. 0 turns the check off. |
| **Scoring Slots per Problem** (`slots_per_problem`) | 2 | How many different contestants can earn a slot on each problem. |
| **Points to Qualify** (`solves_to_qualify`) | 3 | Points needed to qualify directly. The stored setting keeps its original name. |
| **Advancement Places** (`advancement_slots`) | 16 | Total places for direct qualifiers and reserves. |
| **Scoreboard Refresh Interval** (`scoreboard_refresh_seconds`) | 5 | Seconds between automatic refreshes. 0 means manual refresh only. |

Add 20 problems. In each contest problem's **Configure** dialog, choose the
`codelink-qualifier` plugin's `problem` settings. **Points per Scoring Slot**
(`points`) defaults to 1 and accepts only 1 or 2. Set four problems to 2 and
leave the other sixteen at 1. Values belong to this contest, so the same problem
can have a different value in another contest.

The slot limit, point threshold, and advancement places take whole numbers
from 1 to 1000. The expected problem count takes 0 to 1000 and only drives the
setup notice. It does not enforce the problem count or the mix of point values.
The refresh interval takes 0 to 3600.

Existing saved settings keep their values. When reusing an older contest, set
the expected problem count to 20 and the point threshold to 3, and check that
the advancement places are 16.

Set these before the round starts. A saved change takes effect on the next
refresh and recalculates the standings, and it needs no rebuild.

## How contestants qualify

Slots go to accepted submissions in the order they were submitted.

1. Accepted submissions are sorted by submission time. On an exact tie, the
   smaller submission id comes first. The order in which the judge happens to
   finish does not matter.
2. Only a contestant's first accepted submission on each problem counts.
3. That submission takes a free slot on the problem, unless the contestant has
   already qualified.
4. A slot awards the problem's full point value. A contestant qualifies when
   their total reaches or exceeds the point threshold. They keep those slots
   and points. Later accepted submissions take no slots and do not change their
   qualification time.

With the default threshold, both 1 plus 2 points and three separate points
qualify. Two problems worth 2 points each, or 1 plus 1 followed by 2, give a
total of 4 points and also qualify. Direct qualifiers do not receive seed
rankings from this calculation.

An accepted submission that got no slot does not count toward qualifying.
Because qualified contestants are skipped, a slot they would have taken goes to
the next contestant instead. With the default settings this plays out as
follows.

| Event | Result |
| --- | --- |
| Alice earns slots on A worth 1 point and B worth 2 points | Alice qualifies with 3 points and keeps both slots |
| Alice later solves C | Her AC shows, and both C slots stay free |
| Bob and Carol then solve C | Each takes one C slot |
| Dave also solves C | His AC shows without a slot |

Only submissions made during the morning round count. A submission made before
the end still counts if its judging finishes afterwards. Practice submissions
and submissions outside the round take no slots. A failed evaluation, or a
problem with no test cases, never produces an AC.

## During the contest

The current **Contest Homepage** and **Rankings** page have not yet been
adapted for weighted scoring and reserves. The homepage still describes
qualification by problem count. The board's **Credited** column counts
problems, not points, and its qualification status covers direct qualifiers
only. Use the rules on this page and the final API result below for this format.

| Status | Meaning |
| --- | --- |
| **Qualified** | The contestant qualifies however the remaining judging turns out |
| **Awaiting judging** | Unfinished judging could still decide whether the contestant qualifies |

Everyone else has no direct qualification status. The qualifier count and
qualification times include **Qualified** contestants only. They do not
include reserves.

The board refreshes at the configured interval, and keeps refreshing after the
round ends while queued judging finishes. **Auto refresh** pauses it and
**Refresh** updates it right away. With an interval of 0 the page shows
**Manual refresh only**.

## When judging is slow or fails

A contestant shows **Awaiting judging** while their own submission is still
being judged, or while an earlier submission by someone else could still take a
slot they are counting on. Submissions still waiting in the queue count as
unfinished too. Submissions that cannot change anything do not hold anyone
back. That covers attempts on a problem whose slots are certainly full, repeat
attempts on a problem already solved, and attempts by contestants who have
already qualified.

**Qualified** appears as soon as the result is certain, even while other
judging is still running. The slots and times shown next to it can still move
until that judging finishes.

Rejudges and rule changes recalculate the board once applied. A rejudge that
has not been applied yet does not change the official result.

## After the contest

Wait until the round ends, no submission is still being judged, and every
rejudge has been reviewed. All direct qualifiers advance. Fill the remaining
places from contestants with 2 points first, then those with 1 point.

Within each score, the earlier last scoring submission wins the place. An
exact submission time tie is broken by the smaller submission id. A repeated
AC or an AC without a scoring slot does not change this time. Reserves are
selected only after the round and all judging finish, so contestants can keep
earning points during the round.

Read the final selection from the `advancement` field of
`GET /api/plugins/codelink-qualifier/contests/{contest_id}/standings`. It is
`null` until the round ends and all official judging finishes.

| Field | Meaning |
| --- | --- |
| `direct_user_ids` | Contestants who reached the point threshold, listed by user id without seed rankings |
| `reserve_user_ids` | Reserves in selection order, stopping when all places are filled |
| `unfilled_slots` | Places still vacant because too few contestants earned points |

The same response includes each contestant's total in `rows[].score` and each
problem's value in `problems[].points`. `credited` remains the number of
credited problems, and `qualified_count` counts direct qualifiers only.

If `unfilled_slots` is positive, volunteers must arrange a playoff or another
announced selection method. Contestants with no points are not selected
automatically. Applied rejudges recalculate both scores and reserves.

The plugin does not enroll qualifiers anywhere. Enroll the selected contestants
in the [Codelink bracket](./codelink-bracket.md) contest yourself.

The score on an individual submission only says whether that solution passed.
Qualification comes from scoring slots and the final selection above.
