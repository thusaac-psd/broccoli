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

In the contest editor, select `codelink-bracket` as the contest type and add
every problem the bracket uses. Enroll the sixteen players. Then open the
contest's **Configure** dialog and enable the `codelink-bracket` plugin's
`before_submission` check. Without it, players can also submit to problems they
can see but are not currently playing.

## Set up the bracket

Each round needs two groups of three problems and at least one tiebreak problem.
A problem can appear only once in the whole bracket. Send the setup once, before
the first match, as a user with `contest:manage`:

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

| Field | Effect |
| --- | --- |
| `seeds` | Sixteen distinct user ids. Adjacent seeds meet in round 1: the first and second, the third and fourth, and so on. |
| `rounds` | Exactly four rounds. In every match of a round, the first player owns `group_a` and the second owns `group_b`. |
| `xiaoju_seconds` | Time limit for each game. Must be greater than 0. |
| `round_intermission_seconds` | Minimum pause after the last match of a round before a match of the next round can start. |
| `escalation_grace_seconds` | How long a game may wait for a stuck judge before staff must decide the match. Defaults to 120. |

## Play a match

The contest's **Rankings** page shows the bracket. Select a match to see its
details. The **Overview** page shows the rules.

1. Each player ranks the opponent's three problems. The ranking is the order the
   opponent must solve them in.
2. After both rankings are in, staff press **Start match**.
3. In each of the three games, both players work on their next problem at the
   same time. The first accepted submission wins the game. Submission time
   decides, and the smaller submission id breaks an exact tie, the same rule as
   the [Codelink qualifier](./codelink-qualifier.md). If neither player solves
   their problem before the deadline, nobody scores.
4. After three games, the player with more wins advances. A level score,
   including 0-0, opens the round's first tiebreak problem, which both players
   solve. A scoreless tiebreak moves to the next tiebreak problem.

Players see only the problems they have reached. The winner is placed into the
next round automatically.

## Handle judging problems

A game is never decided while an earlier submission is still being judged, even
after its deadline. The match shows **Waiting on a pending submission** and
names the blocking submission. When the result arrives, the game resolves
normally.

If the judge does not finish within `escalation_grace_seconds`, the match moves
to **Needs staff decision**. Check the named submission's result, rejudge it if
it is stuck, and then use **Award to** to decide the match. On a match that is
still running, **Force expiry** re-checks the current game right away, for
example after a rejudge. It does not end a game before its deadline.
