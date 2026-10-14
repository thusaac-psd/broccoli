---
title: Codelink bracket
sidebar_label: Codelink bracket
sidebar_position: 3
---

# Codelink bracket

The `codelink-bracket` contest type runs the Codelink afternoon round. Sixteen
players, usually the qualifiers from the
[Codelink qualifier](./codelink-qualifier.md), meet in a single elimination
bracket of four rounds. A match is three games, each won by whoever solves
their problem first, plus tiebreak games if the score is level.

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

1. In the contest editor, choose `codelink-bracket` as the contest type and
   set the start and end of the afternoon round. Leave enough time for four
   rounds, since no match can be played after the end.
2. Add every problem the bracket uses. Each round needs seven or more, as the
   next section explains.
3. Enroll the sixteen players.

## Configure the rules

Open the contest's **Rankings** page. Until the bracket exists, staff see the
setup screen there.

1. **Players and seeding.** The first sixteen enrolled players are listed in
   registration order. Drag them into seed order, or use **Shuffle**. Seed 1
   plays seed 2 in round 1, seed 3 plays seed 4, and so on. Remove a player to
   bring in someone from the other enrolled players.
2. **Problems per round.** Every round needs three problems for the first
   player of each match, three for the second, and at least one tiebreak
   problem. The slots start out filled in the contest's problem order. A
   problem can appear only once in the whole bracket.
3. **Timing per stage.** Set the regular game duration, tiebreak duration,
   and break before each stage. The form starts with 30 minutes for both game
   types and 10 minutes for later stage breaks. Round one opens with the
   contest and has no preceding break. Penalty seconds and the wait for a stuck
   judge apply to the whole contest, with defaults of 20 and 120 seconds.

**Create bracket** checks all of this and lists anything missing. The setup
screen closes after creation. Staff can submit a corrected setup through the
API. A match keeps both game durations from when its first game opened.
Corrections to durations apply to matches that have not started. Stage breaks
remain live. Existing matches keep their players and assigned problem groups.

To script the setup instead, send the same data to the setup route as a user
with `contest:manage`.

```bash
curl -X POST \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  "$BROCCOLI/api/v1/p/codelink-bracket/api/plugins/codelink-bracket/contests/$CONTEST/setup" \
  -d '{
    "seeds": [11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26],
    "rounds": [
      { "group_a": [101, 102, 103], "group_b": [104, 105, 106], "tiebreak": [107, 108],
        "xiaoju_seconds": 1200, "tiebreak_seconds": 1200, "break_before_seconds": 0 },
      { "group_a": [111, 112, 113], "group_b": [114, 115, 116], "tiebreak": [117],
        "xiaoju_seconds": 1800, "tiebreak_seconds": 2100, "break_before_seconds": 600 },
      { "group_a": [121, 122, 123], "group_b": [124, 125, 126], "tiebreak": [127],
        "xiaoju_seconds": 2400, "tiebreak_seconds": 2700, "break_before_seconds": 600 },
      { "group_a": [131, 132, 133], "group_b": [134, 135, 136], "tiebreak": [137],
        "xiaoju_seconds": 3000, "tiebreak_seconds": 3600, "break_before_seconds": 900 }
    ],
    "penalty_seconds": 20,
    "escalation_grace_seconds": 120
  }'
```

| Field | Effect |
| --- | --- |
| `seeds` | Sixteen distinct user ids in seed order. The first plays the second in round 1, the third plays the fourth, and so on. |
| `rounds` | Exactly four rounds. In every match of a round, the first player gets `group_a` and the second gets `group_b`. |
| `rounds[].xiaoju_seconds` | Required positive duration for each of the stage's three regular games, in seconds. |
| `rounds[].tiebreak_seconds` | Required positive duration for each tiebreak game in that stage, in seconds. |
| `rounds[].break_before_seconds` | Required nonnegative break after every match of the preceding stage finishes. Must be 0 for round one. |
| `penalty_seconds` | Delay added by each wrong submission within a game. Defaults to 20. Set to 0 to disable the delay. |
| `escalation_grace_seconds` | How long a game waits for a stuck judge before staff must decide. Defaults to 120. |

## How a match is won

1. **Ranking.** Each player drags the opponent's three problems into the order
   the opponent must solve them in. A ranking can be changed until the match
   starts.
2. **Start.** Round one opens with the contest. Later stages open after all
   matches of the preceding stage finish and the next stage's break elapses.
   Players can rank only after their stage opens. Each match then starts as
   soon as both rankings are in. There is no ranking deadline.
3. **Games.** In each of the three games, both players work on their next
   problem at the same time. The first accepted submission wins the game.
   Effective submission time includes accumulated penalties for that game.
   Judge completion order does not determine the winner. The smaller
   submission id breaks an exact tie. If neither player solves their
   problem before the game ends, nobody scores. The next game opens as soon as
   one is decided.
4. **Result.** After three games the player with more wins advances. On a
   level score, including when nobody scored, both players get the round's
   first tiebreak problem. A scoreless tiebreak moves on to the next tiebreak
   problem.

Winners wait together for the next stage to open. Every match in a stage uses
that stage's problems and durations. A tiebreak without a winner uses the next
problem with the same tiebreak duration. Matches can progress at different
speeds within an open stage, so run the contest in a supervised room.

## During the contest

Players work from the **Contest Homepage**. It opens with their own match,
showing where they are, the ranking list when it is their turn to rank, and
their current problem with its clock once a game is live. A player sees only
the problems they have reached and can submit only to the one they are
currently playing. The **Rankings** page shows them a short link
back to their match.

The **Rankings** page shows the whole bracket as a tree. Each card shows the
seeds, one dot per game, and the clock of the live game. A waiting match shows
when it will start. **Present** puts the bracket full screen for an audience
and hides the staff counts. Select a match to open its panel with the score,
each game's problems and winner, both players' submissions with links to their
judging, and the staff controls.

Staff can act from the match panel. Every action asks for confirmation first.

| Control | When it appears | What it does |
| --- | --- | --- |
| **Start now** | The match is waiting for rankings | Starts the match within an open stage. A missing ranking becomes the listed order. It cannot bypass the stage opening. |
| **Force expiry** | A game is live or waiting on judging | Checks the current game again right away, for example after a rejudge. It never ends a game before its time is up. |
| **Award to** | A game is live, waiting on judging, or the match needs a staff decision | Gives the match to that player, who moves into the next round. This cannot be undone. |

## When judging is slow or fails

A game is not decided while an earlier submission is still being judged, even
after its time is up. The match shows **Waiting on a pending submission**. When
the result arrives, the game is decided as usual.

If judging has not finished after `escalation_grace_seconds`, the match changes
to **Needs staff decision**, and the summary above the bracket counts it. The
match panel says why, links to the stuck submission, and can rejudge it. Check
the result, then use **Award to**. A match also needs a staff decision when its
tiebreak problems run out without a winner.

## After the contest

No match starts at or after the contest's end time, whether on its own or with
**Start now**. A game still running at the end cannot be won any more, because
nobody can submit. It is not scored as a draw. The match goes to **Needs staff
decision** and the panel says the contest ended first. A submission still being
judged at that moment is waited for, as it would be at any game's end.

The summary above the bracket warns staff before this happens, counting the
matches whose live game or planned start falls after the end. Once the end has
passed, it counts the matches still unfinished. Award each one to finish the
bracket.
