---
title: Codelink bracket
sidebar_label: Codelink bracket
sidebar_position: 3
---

# Codelink bracket

The `codelink-bracket` contest type runs a single elimination bracket for
contestants, usually the qualifiers from the
[Codelink qualifier](./codelink-qualifier.md). Choose a bracket size from 2 to
128 players in powers of two. Each stage sets a maximum number of regular
games, called BO, and the wins needed to advance, called FT. A player who
reaches FT wins the match immediately. At the BO game cap, the higher score
advances and a level score goes to tiebreak games.

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

## Receive game reminders

Select **Enable desktop notifications** on an afternoon contest page and allow
notifications in your browser. Select **Test notification** before playing to
check your desktop notification settings. The site must use HTTPS or localhost.

Both players receive a result when each regular game or tiebreak game ends.
The message says whether you won, your opponent won, or time expired without
either player scoring. It also shows the score after that game.

Game results stay on the page until you dismiss them. When the contest tab is
hidden or its window is out of focus, an enabled desktop notification also
appears. Click it to return to the contest. Keep a contest page open while
working in another application. Changing contest pages keeps the reminders
connected, and a brief disconnection recovers missed results from the game history.
Opening a contest for the first time does not replay its previous results.

If you denied notification permission, allow it in the browser's site settings.
If the test does not appear, check the desktop's notification and do not disturb
settings. A closed or suspended browser page cannot receive these reminders.

## Create the contest

1. In the contest editor, choose `codelink-bracket` as the contest type and
   set the start and end of the afternoon round. Leave enough time for all
   stages, since no match can be played after the end.
2. Add every problem the bracket uses. Each stage needs two equally sized problem
   groups and at least one tiebreak problem.
3. Enroll the players you plan to seed.

## Configure the rules

Open the **Contest Homepage**, then select **Schedule and seeding** under
**Actions**. You can also open it from the contest's actions menu in the
management list. This opens **Tournament management**, where volunteers with
`contest:manage` configure the bracket and stage schedule. Until configuration
is complete, **Rankings** shows that the bracket has not been created.

1. **Players and seeding.** Choose the bracket size, initially 16. The stage
   count follows the size. The selected players start in registration order.
   Drag them into seed order, or use **Shuffle**. Seed 1 plays seed 2 in round
   one, seed 3 plays seed 4, and so on. Remove a player to bring in someone
   from the other enrolled players.
2. **Problems and win target per stage.** Set **Maximum regular games (BO)**
   and **Wins to advance (FT)**. New stages start with BO3 and FT2, so a score
   of 2 to 0 ends the match after two games. FT must be a whole number from
   1 to BO. Each player needs BO problems, plus at least one shared tiebreak problem.
   The two groups must have equal lengths. A stage may have at most 256 games
   including its tiebreak list. The slots start out filled in the contest's
   problem order. A problem can appear only once in the configured groups. The bronze match can explicitly share the final groups.
3. **Timing per stage.** Set each stage's ordering start using the date and time
   input. The displayed timezone is your browser's local timezone. Every match
   in that stage receives the same 3 minute ordering window, then a preparation
   wait of 30 seconds by default. Set the game base duration, tiebreak duration,
   shared extra time and game break for that stage. The game break defaults to
   60 seconds and starts when the previous game is settled. Later stages also
   require a minimum break after the whole preceding stage, initially 10 minutes.
   Round one has no preceding stage break. Regular game errors add 20 seconds
   by default. Tiebreak errors add no penalty. The judge wait defaults to 120 seconds.
4. **Bronze match.** Enable the bronze match before the contest starts. The two
   semifinal winners enter the championship and the two losers enter the bronze
   match. Both use the final stage's clock, BO, FT and durations. The bronze match
   shares the final's problems by default. Select separate groups to use other
   problems with the same BO count.

**Create bracket** checks the configuration and lists anything missing. Before
the contest starts, you can change its size, seeding and stage rules. Once the
contest starts, size, seeding and shared settings are fixed. Each stage's
problems, BO, FT and timing are fixed when that stage opens,
including while its contestants are still ranking.

To script the setup instead, send the same data to the setup route as a user
with `contest:manage`. Replace the example UTC millisecond timestamps with future times inside your contest window.

```bash
curl -X POST \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  "$BROCCOLI/api/v1/p/codelink-bracket/api/plugins/codelink-bracket/contests/$CONTEST/setup" \
  -d '{
    "seeds": [11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26],
    "rounds": [
      { "group_a": [101, 102, 103], "group_b": [104, 105, 106], "tiebreak": [107, 108],
        "start_at_ms": 1893499200000, "post_order_wait_seconds": 30, "game_break_seconds": 60,
        "match_extra_time_seconds": 300,
        "wins_to_advance": 2, "xiaoju_seconds": 1200, "tiebreak_seconds": 1200, "break_before_seconds": 0 },
      { "group_a": [111, 112, 113, 114, 115], "group_b": [116, 117, 118, 119, 120], "tiebreak": [129],
        "start_at_ms": 1893506400000, "post_order_wait_seconds": 30, "game_break_seconds": 60,
        "match_extra_time_seconds": 300,
        "wins_to_advance": 3, "xiaoju_seconds": 1800, "tiebreak_seconds": 2100, "break_before_seconds": 600 },
      { "group_a": [121, 122, 123], "group_b": [124, 125, 126], "tiebreak": [127],
        "start_at_ms": 1893513600000, "post_order_wait_seconds": 30, "game_break_seconds": 60,
        "match_extra_time_seconds": 300,
        "wins_to_advance": 2, "xiaoju_seconds": 2400, "tiebreak_seconds": 2700, "break_before_seconds": 600 },
      { "group_a": [131, 132, 133], "group_b": [134, 135, 136], "tiebreak": [137],
        "start_at_ms": 1893520800000, "post_order_wait_seconds": 30, "game_break_seconds": 60,
        "match_extra_time_seconds": 300,
        "wins_to_advance": 2, "xiaoju_seconds": 3000, "tiebreak_seconds": 3600, "break_before_seconds": 900 }
    ],
    "third_place": { "mode": "shared" },
    "match_extra_time_seconds": 300,
    "penalty_seconds": 20,
    "escalation_grace_seconds": 120
  }'
```

| Field | Effect |
| --- | --- |
| `seeds` | A power of two from 2 to 128 distinct user ids in seed order. The first plays the second in round 1, the third plays the fourth, and so on. |
| `rounds` | One round per elimination stage, so eight players need three rounds and sixteen need four. In every match of a round, the first player gets `group_a` and the second gets `group_b`. |
| `rounds[].group_a`, `rounds[].group_b` | Equally sized nonempty problem lists. Their length is the maximum regular game count BO for the stage. |
| `rounds[].wins_to_advance` | FT target from 1 to BO. The first player to reach it advances immediately. Omission or `null` uses half the BO count rounded down, plus 1. |
| `rounds[].xiaoju_seconds` | Required positive base duration for each of the stage's regular games, in seconds. |
| `rounds[].tiebreak_seconds` | Required positive duration for each tiebreak game in that stage, in seconds. |
| `rounds[].break_before_seconds` | Required nonnegative break after every match of the preceding stage finishes. Must be 0 for round one. |
| `rounds[].start_at_ms` | UTC milliseconds when the whole stage begins ordering. New or changed times must be in the future, inside the contest window, and increasing by stage. |
| `rounds[].post_order_wait_seconds` | Preparation wait after the full 180 second ordering window. Defaults to 30. |
| `rounds[].game_break_seconds` | Break after a settled game before the next regular game or tiebreak. Defaults to 60 for scheduled stages. |
| `rounds[].match_extra_time_seconds` | Regular game allowance per match for this stage. Omission uses the root fallback. |
| `third_place` | `null` disables the bronze match. `{"mode":"shared"}` shares the final problems. `{"mode":"custom","group_a":[...],"group_b":[...],"tiebreak":[...]}` selects separate groups. |
| `match_extra_time_seconds` | Fallback extra time shared by the regular games of each match. Must be nonnegative. Defaults to 0. |
| `penalty_seconds` | Score penalty added by each wrong submission within a regular game. Defaults to 20. Set to 0 to disable. Tiebreaks have no error penalties. |
| `escalation_grace_seconds` | How long a game waits for a stuck judge before staff must decide. Defaults to 120. |

### Edit future stages

Open **Schedule and seeding** from the contest's management actions to change
any stage that has not opened. You can change its BO, FT, problem lists, game durations
and breaks. Select **Save changes** to apply the edit. At the configured start,
the preceding stage must be fully decided and its minimum rest completed. If
either condition was unmet, the stage shows **Reschedule required** and stays
closed until you assign a new future time. It never silently shifts its clock.
A delayed or duplicate timer preserves the original clock when prerequisites
were satisfied at the configured instant. Opened stages stay disabled in the editor.
If a stage opens while you are editing, the server rejects changes to it.

The setup API accepts the same edits through the complete setup request. Use
`GET` on the setup route with `contest:manage` to read the current configuration
and its `opened_rounds` count. Bracket size can change only before the contest
starts. During play you cannot add or remove elimination stages.

Existing configured brackets are upgraded when the plugin reloads. Their
seeding, games, scores and clocks are preserved. Opened stages keep their
previous rule of playing all regular games. Unopened stages without an FT
target receive the majority default and remain editable. Stages without
`start_at_ms` retain their legacy pacing. Add schedules to unopened stages to
use the shared ordering and preparation windows.

### Shared extra time

Set **Shared extra time per match** within each stage to give its matches an allowance shared by
both contestants across its regular games. When base time runs out,
extra time starts automatically and continues until someone wins or the
allowance runs out. Contestants cannot skip extra time to save it for later.
If neither contestant solves the game, it uses all remaining extra time and
nobody scores. Later games then have only their base duration.

For example, with 10 minutes per game and 5 shared extra minutes, a winning
submission at minute 12 uses 2 extra minutes. The next game has its usual
10 minutes plus at most 3 extra minutes. Unused base time does not increase
the allowance. Each new match receives a fresh allowance, and tiebreak games
use their stage's configured tiebreak duration without shared extra time.

Extra time is settled using the winning submission's actual submission time.
Neither score penalties nor waiting for its judge result consume extra time.
A submission must arrive before the final deadline. It remains eligible even
if adding its penalty puts its score time beyond that deadline.
Opened stages and running matches keep their timing when future stages change.

## How a match is won

1. **Ranking.** At the stage's configured start, each player ranks the
   opponent's problems. The window lasts the full 3 minutes even if everyone
   submits early. The last valid ranking wins. A missing ranking becomes the
   group's listed order at the deadline.
2. **Start.** After ordering closes, everyone waits the configured preparation
   duration, initially 30 seconds. All first games in that stage start together.
3. **Games.** In each regular game, both players work on their next
   problem at the same time. Accepted submissions are compared using actual
   submission time plus accumulated penalties. The lowest score time wins.
   Judge completion order does not determine the winner. The smaller
   submission id breaks an exact tie. If neither player solves their
   problem before the game ends, nobody scores. If the match continues, the next
   game opens after the configured game break, initially 60 seconds. This
   applies to regular games and consecutive tiebreaks. Tiebreaks do not repeat
   ordering or preparation. The final game adds no extra game break.
4. **Result.** After each regular game, a player who has reached FT advances
   immediately, and the remaining regular games are skipped. Otherwise play
   continues up to BO games. At that cap, the player with more wins advances
   even if neither reached FT. On a level score, including when nobody scored,
   both players get the round's first tiebreak problem. A scoreless tiebreak
   moves on to the next tiebreak problem.

For BO3 and FT2, scores of 2 to 0 and 2 to 1 end the match. A scoreless game
uses one of the three games without adding a win. After three games, 1 to 0
advances and 1 to 1 or 0 to 0 enters a tiebreak. BO and FT can be set
independently, such as BO5 and FT3. Tiebreak games do not count toward BO.

Tiebreaks compare the actual submission times of accepted solutions. Wrong
submissions add no penalty, regardless of the regular game penalty setting.
The smaller submission id breaks an exact tie.

Once an accepted submission exists, the game waits until no future submission
can beat its score, or until the real deadline. It then waits for any pending
judging that could change the winner. Score penalties do not shorten the
submission window or consume shared extra time.

Winners wait together for the next stage to open. Every match in a stage uses
that stage's problems and durations. A tiebreak without a winner uses the next
problem with the same tiebreak duration. Matches can progress at different
speeds within an open stage, so run the contest in a supervised room.

## During the contest

Players work from the **Contest Homepage**. It opens with their own match,
showing where they are, the ranking list when it is their turn to rank, and
their current problem with its clock once a game is live. A player sees only
the opponent's group during ordering. During play, only the current own problem
is visible, or the shared tiebreak. Problem letters are hidden. Preparation,
breaks and decided matches expose no statements. Previous URLs, samples and
attachments are denied by the server too. Statement caches are cleared when
the phase changes while editor drafts remain. The **Rankings** page shows them a short link
back to their match.

The **Rankings** page shows mirrored left and right halves expanding outward
from the center. The championship sits above the bronze match in the middle.
Solid lines carry winners and dashed lines carry semifinal losers. The header
shows only the current stage and its remaining matches. The schedule overview
and editing controls are on **Tournament management**. Each match card shows the
seeds, one dot per game, and the clock of the live game. A waiting match shows
when it will start. **Present** puts the bracket full screen for an audience
and hides the staff counts. Select a match to open its panel with the score,
each game's problems and winner, both players' submissions with links to their
judging, and the staff controls.

Staff can act from the match panel. Every action asks for confirmation first.

| Control | When it appears | What it does |
| --- | --- | --- |
| **Start now** | A legacy unscheduled match is waiting for rankings | Starts that match within an open stage. Scheduled stages have no individual start override. |
| **Force expiry** | A game is live or waiting on judging | Checks the current game again right away, for example after a rejudge. It never ends a game before its time is up. |
| **Award to** | A game is live, waiting on judging, or the match needs a staff decision | Gives the match to that player, who moves into the next round. This cannot be undone. |

### Game reminders

Game results appear as persistent web notifications until dismissed. Select
**Enable desktop notifications**, then **Test notification** to check browser
and system permissions. When the contest window is in the background, game
results also request a desktop notification that remains available in the
system notification center. The operating system controls banner duration.
The reminders reconnect automatically and do not replay already seen results.

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
