import assert from 'node:assert/strict';
import { test } from 'node:test';

import { makeMatch } from './fixtures.test-util.ts';
import {
  adjudicationMessage,
  contestEndWarning,
  currentRoundProgress,
  roundsInPlay,
} from './summary.ts';

const decided = (round: number) => makeMatch({ round, state: 'decided' });
const completedThrough = (last: number) =>
  Array.from({ length: last }, (_, index) => {
    const round = index + 1;
    return Array.from(
      { length: 2 ** (4 - round) + Number(round === 4) },
      (_, pos) => makeMatch({ round, pos, state: 'decided' }),
    );
  }).flat();

test('current-stage count ignores completed earlier rounds and includes breaks and judge waits', () => {
  const matches = [
    ...completedThrough(2),
    decided(3),
    makeMatch({ round: 3, state: 'intermission' }),
  ];
  assert.deepEqual(currentRoundProgress(matches, 4, true), {
    round: 3,
    remaining: 1,
  });
  matches.at(-1)!.state = 'awaiting_judge';
  assert.deepEqual(currentRoundProgress(matches, 4, true), {
    round: 3,
    remaining: 1,
  });
});

test('an unopened next stage counts its pending pairings and final stage includes bronze', () => {
  assert.deepEqual(currentRoundProgress(completedThrough(1), 4, true), {
    round: 2,
    remaining: 4,
  });
  assert.deepEqual(
    currentRoundProgress(
      [
        ...completedThrough(3),
        decided(4),
        makeMatch({ round: 4, pos: 1, kind: 'third_place', state: 'tiebreak' }),
      ],
      4,
      true,
    ),
    { round: 4, remaining: 1 },
  );
  assert.equal(currentRoundProgress(completedThrough(4), 4, true), null);
});

test('partially created stages keep uncreated matches in their remaining count', () => {
  assert.deepEqual(currentRoundProgress([decided(1), decided(1)], 4, true), {
    round: 1,
    remaining: 6,
  });
});

test('rounds in play lists every round with an unfinished match', () => {
  const matches = [
    decided(1),
    makeMatch({ round: 1, state: 'in_progress' }),
    makeMatch({ round: 2, state: 'ordering' }),
  ];
  assert.deepEqual(roundsInPlay(matches), [1, 2]);
});

test('with every created match decided, the next round is in play', () => {
  assert.deepEqual(roundsInPlay([decided(1), decided(1)]), [2]);
});

test('no round is in play once the tournament is over', () => {
  const all = Array.from({ length: 15 }, () => decided(1));
  assert.deepEqual(roundsInPlay(all), []);
});

test('the adjudication message follows the recorded reason', () => {
  const m = (reason: Parameters<typeof makeMatch>[0]) =>
    adjudicationMessage(makeMatch({ state: 'needs_adjudication', ...reason }));
  assert.equal(
    m({ adjudication_reason: 'contest_ended', awaiting_submission_id: 5 }).key,
    'codelink-bracket.match.contestEnded',
  );
  assert.deepEqual(
    m({ adjudication_reason: 'stuck_judge', awaiting_submission_id: 5 }),
    { key: 'codelink-bracket.match.stuckJudge', params: { id: 5 } },
  );
  assert.equal(
    m({ adjudication_reason: 'tiebreak_exhausted' }).key,
    'codelink-bracket.match.tiebreakExhausted',
  );
  assert.equal(
    m({ adjudication_reason: 'setup_missing' }).key,
    'codelink-bracket.match.setupMissing',
  );
});

test('matches escalated before reasons existed fall back to the old inference', () => {
  const m = (id: number | null) =>
    adjudicationMessage(
      makeMatch({ state: 'needs_adjudication', awaiting_submission_id: id }),
    ).key;
  assert.equal(m(9), 'codelink-bracket.match.stuckJudge');
  assert.equal(m(null), 'codelink-bracket.match.tiebreakExhausted');
});

test('no contest-end warning without an end time or with time to spare', () => {
  const live = makeMatch({
    state: 'in_progress',
    current_xiaoju_deadline_ms: 1_000,
  });
  assert.equal(contestEndWarning([live], null, 0), null);
  assert.equal(contestEndWarning([live], 5_000, 0), null);
});

test('games and starts past the contest end are counted as cut off', () => {
  const matches = [
    makeMatch({ state: 'in_progress', current_xiaoju_deadline_ms: 6_000 }),
    makeMatch({ state: 'ordering', starts_at_ms: 5_000 }),
    makeMatch({ state: 'ordering', starts_at_ms: 4_000 }),
    makeMatch({ state: 'ordering', starts_at_ms: null }),
  ];
  assert.deepEqual(contestEndWarning(matches, 5_000, 0), {
    kind: 'cutoff',
    count: 2,
  });
});

test('after the contest end every unfinished match counts', () => {
  const matches = [decided(1), decided(1)];
  assert.deepEqual(contestEndWarning(matches, 5_000, 5_000), {
    kind: 'ended',
    unfinished: 15 - 2,
  });
  const all = Array.from({ length: 15 }, () => decided(1));
  assert.equal(contestEndWarning(all, 5_000, 9_000), null);
});

test('an eight-player final completes seven matches', () => {
  const all = Array.from({ length: 7 }, () =>
    makeMatch({ stage_count: 3, state: 'decided' }),
  );
  assert.deepEqual(roundsInPlay(all), []);
  assert.equal(contestEndWarning(all, 5000, 9000), null);
});
