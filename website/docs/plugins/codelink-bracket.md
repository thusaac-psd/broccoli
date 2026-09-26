---
title: Codelink bracket
sidebar_label: Codelink bracket
sidebar_position: 3
---

# Codelink bracket

Use the `codelink-bracket` contest type for the afternoon knockout round. Sixteen
players meet in a single-elimination bracket of four rounds. Each match is three
games plus tiebreaks if needed, and each game is won by the earlier accepted
submission.

## Build and enable the plugin

Run these commands from the Broccoli repository after installing its development
dependencies.

```bash
pnpm --filter @broccoli/web-sdk build
just build-plugin plugins/codelink-bracket --install
```

The build produces `plugins/codelink-bracket/codelink_bracket.wasm` and the
frontend bundle under `plugins/codelink-bracket/frontend/dist`. Start the server
or use **Reload all plugins** in the admin area to discover it.

In the contest editor, select `codelink-bracket` as the contest type, add every
problem the bracket uses, and enroll the sixteen players.

## Set up the bracket

Open the contest's **Rankings** page. Until the bracket exists, staff see the
setup screen there.

1. **Players and seeding.** The first sixteen enrolled players are listed in
   registration order. Drag them into seed order, or use **Shuffle**.
   Neighbours meet in round 1: seed 1 plays seed 2, seed 3 plays seed 4, and so
   on. Remove a player to swap in someone from the list of other enrolled
   players.
2. **Problems per round.** Each round needs three problems for the first player
   of every match, three for the second, and at least one tiebreak problem. The
   slots are filled from the contest's problem order to start with. A problem
   can appear only once in the whole bracket.
3. **Timing.** Set the game length, the break each player gets between their
   matches, and how long a game may wait on a stuck judge before staff must
   decide the match.

**Create bracket** checks all of this first and lists anything missing. Creating
the bracket also turns on the plugin's `before_submission` check for the
contest, which stops players submitting to problems they are not currently
playing. Seeding and problems cannot be changed afterwards.

To script the setup instead, send the same data to the setup route as a user
with `contest:manage`:

```bash
curl -X POST \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  "$BROCCOLI/api/v1/p/codelink-bracket/api/plugins/codelink-bracket/contests/$CONTEST/setup" \
  -d '{
    "seeds": [11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26],
    "rounds": [
      { "group_a": [101, 102, 103], "group_b": [104, 105, 106], "tiebreak": [107, 108] },
      { "group_a": [111, 112, 113], "group_b": [114, 115, 116], "tiebreak": [117] },
      { "group_a": [121, 122, 123], "group_b": [124, 125, 126], "tiebreak": [127] },
      { "group_a": [131, 132, 133], "group_b": [134, 135, 136], "tiebreak": [137] }
    ],
    "xiaoju_seconds": 1800,
    "round_intermission_seconds": 600,
    "escalation_grace_seconds": 120
  }'
```

A scripted setup does not enable the `before_submission` check. Enable it in the
contest's **Configure** dialog.

| Field | Effect |
| --- | --- |
| `seeds` | Sixteen distinct user ids. Adjacent seeds meet in round 1: the first and second, the third and fourth, and so on. |
| `rounds` | Exactly four rounds. In every match of a round, the first player owns `group_a` and the second owns `group_b`. |
| `xiaoju_seconds` | Time limit for each game. Must be greater than 0. |
| `round_intermission_seconds` | Break each player gets after their own match before their next match can start. |
| `escalation_grace_seconds` | How long a game may wait for a stuck judge before staff must decide the match. Defaults to 120. |

## Play a match

The **Rankings** page shows the bracket as a tree. Each card shows the seeds,
one dot per game (who won it, or which game is live), and the live game's
clock. **Present** shows the bracket full screen for an audience and hides the
staff-only counts. Select a match to open its details in a side panel: the
score, each game's problems and winner, both players' submissions with links
to their judging, and the staff controls.

Contestants work from the contest's **Overview** page. It opens with their own
match: a stepper for where they are, the drag-to-rank list when it is their
turn to rank, and the current problem with its clock once a game is live. The
Rankings page shows them a one-line link back to it.

1. Each player drags the opponent's three problems into the order the opponent
   must solve them.
2. The match starts by itself as soon as both players have ranked, the contest
   has started, and both players have had their break since their previous
   match. There is no ranking deadline: a match waits until both rankings are
   in. The bracket shows each waiting match's countdown.
3. In each of the three games, both players work on their next problem at the
   same time. The first accepted submission wins the game. Submission time
   decides, and the smaller submission id breaks an exact tie, the same rule as
   the [Codelink qualifier](./codelink-qualifier.md). If neither player solves
   their problem before the deadline, nobody scores.
4. After three games, the player with more wins advances. A level score,
   including 0-0, opens the round's first tiebreak problem, which both players
   solve. A scoreless tiebreak moves to the next tiebreak problem.

Matches do not wait for the rest of their round, so the winner of a quick match
can start the next round while other matches are still running. Every match in
a round uses the same problems, so a pair that starts later faces problems that
earlier pairs have already seen. Run the round in a supervised room.

If a player never ranks, staff can use **Start now** in the match panel. A
missing ranking becomes the problems' listed order, and any remaining break is
skipped.

Players see only the problems they have reached. The winner is placed into the
next round automatically.

## Handle judging problems

A game is never decided while an earlier submission is still being judged, even
after its deadline. The match shows **Waiting on a pending submission** and
names the blocking submission. When the result arrives, the game resolves
normally.

If the judge does not finish within `escalation_grace_seconds`, the match moves
to **Needs staff decision**, and the summary above the bracket counts it. The
match panel links to the named submission and can rejudge it. Check the result,
then use **Award to** to decide the match. On a match that is still running,
**Force expiry** re-checks the current game right away, for example after a
rejudge. It does not end a game before its deadline. Every staff action asks
for confirmation first.
