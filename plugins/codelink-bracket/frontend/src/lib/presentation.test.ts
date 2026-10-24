import assert from 'node:assert/strict';
import { test } from 'node:test';

import { makeMatch } from './fixtures.test-util.ts';
import { matchPresentation } from './presentation.ts';

test('ordering exposes the opponents group; buffers expose no problem', () => {
  const match = makeMatch();
  assert.deepEqual(matchPresentation(match, 'a'), {
    hideLabels: false,
    allowedProblemIds: [201, 202, 203],
  });
  for (const state of ['preparing', 'intermission', 'decided'] as const) {
    assert.deepEqual(matchPresentation({ ...match, state }, 'a'), {
      hideLabels: true,
      allowedProblemIds: [],
    });
  }
});

test('only the current own problem survives a transition, including awaiting judge', () => {
  const match = makeMatch({
    state: 'in_progress',
    current_xiaoju_index: 1,
    games: [
      {
        index: 0,
        tiebreak: false,
        problem_a: 101,
        problem_b: null,
        opened_at_ms: 0,
        deadline_ms: 10,
        winner: 10,
        decided: true,
      },
      {
        index: 1,
        tiebreak: false,
        problem_a: 102,
        problem_b: null,
        opened_at_ms: 70,
        deadline_ms: 80,
        winner: null,
        decided: false,
      },
    ],
  });
  assert.deepEqual(matchPresentation(match, 'a').allowedProblemIds, [102]);
  assert.deepEqual(
    matchPresentation({ ...match, state: 'awaiting_judge' }, 'a')
      .allowedProblemIds,
    [102],
  );
  assert.deepEqual(matchPresentation(match, 'b').allowedProblemIds, []);
  assert.deepEqual(
    matchPresentation(undefined, undefined).allowedProblemIds,
    [],
  );
});

test('shared championship and third-place problems still follow their own match phase', () => {
  const game = {
    index: 3,
    tiebreak: true,
    problem_a: 301,
    problem_b: 301,
    opened_at_ms: 0,
    deadline_ms: 10,
    winner: null,
    decided: false,
  };
  const final = makeMatch({
    kind: 'final',
    state: 'tiebreak',
    current_xiaoju_index: 3,
    games: [game],
  });
  const bronze = makeMatch({
    kind: 'third_place',
    state: 'intermission',
    current_xiaoju_index: 3,
    games: [game],
  });
  assert.deepEqual(matchPresentation(final, 'b').allowedProblemIds, [301]);
  assert.deepEqual(matchPresentation(bronze, 'b').allowedProblemIds, []);
});
