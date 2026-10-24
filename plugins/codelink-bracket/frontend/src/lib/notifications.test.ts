import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { GameView } from '../types.ts';
import { makeMatch } from './fixtures.test-util.ts';
import { gameNotificationText, gameUpdates } from './notifications.ts';

function game(index: number, winner: number | null, decided = true): GameView {
  return {
    index,
    winner,
    decided,
    tiebreak: index >= 3,
    opened_at_ms: 1000 + index,
    deadline_ms: 2000 + index,
    problem_a: 101,
    problem_b: 201,
  };
}

test('first snapshot establishes a baseline without replaying history', () => {
  const matches = [makeMatch({ games: [game(0, 10)] })];
  const first = gameUpdates(matches, 10, null);
  assert.deepEqual(first.notifications, []);
  assert.equal(first.seen.length, 1);
  assert.deepEqual(gameUpdates(matches, 10, first.seen).notifications, []);
});

test('each small game notifies both players before the whole match ends', () => {
  const matches = [
    makeMatch({
      state: 'in_progress',
      games: [game(0, 10), game(1, null, false)],
    }),
  ];
  const own = gameUpdates(matches, 10, []).notifications;
  const opponent = gameUpdates(matches, 20, []).notifications;
  assert.equal(own.length, 1);
  assert.equal(own[0].result, 'won');
  assert.deepEqual(own[0].score, [1, 0]);
  assert.equal(opponent[0].result, 'lost');
  assert.deepEqual(opponent[0].score, [0, 1]);
  assert.deepEqual(gameUpdates(matches, 30, []).notifications, []);
});

test('deadlines and match-level staff decisions do not fabricate small-game results', () => {
  for (const state of [
    'in_progress',
    'awaiting_judge',
    'needs_adjudication',
    'decided',
  ] as const) {
    const match = makeMatch({ state, games: [game(0, null, false)] });
    assert.deepEqual(gameUpdates([match], 10, []).notifications, []);
  }
});

test('reconnection catches every missed game and the previous round final result', () => {
  const previous = makeMatch({
    state: 'decided',
    games: [game(0, 10), game(1, null), game(2, 20)],
  });
  const next = makeMatch({ id: 8, round: 2, games: [game(0, 10)] });
  const update = gameUpdates([next, previous], 10, []);
  assert.deepEqual(
    update.notifications.map((n) => n.result),
    ['won', 'timeout', 'lost', 'won'],
  );
  assert.deepEqual(
    update.notifications.map((n) => n.score),
    [
      [1, 0],
      [1, 0],
      [1, 1],
      [1, 0],
    ],
  );
  const restored = JSON.parse(JSON.stringify(update.seen)) as string[];
  assert.deepEqual(
    gameUpdates([previous, next], 10, restored).notifications,
    [],
  );
});

test('tiebreak numbering follows the configured game count and final games still notify', () => {
  const match = makeMatch({
    state: 'decided',
    group_a: [1, 2, 3, 4, 5],
    games: [game(5, 20)],
  });
  const [notice] = gameUpdates([match], 10, []).notifications;
  assert.deepEqual(notice.label, ['codelink-bracket.game.tiebreak', { n: 1 }]);
  const t = (key: string, params?: Record<string, string | number>) =>
    `${key}:${JSON.stringify(params)}`;
  assert.match(gameNotificationText(notice, t).body, /notifications.lost/);
});

test('a new game opening distinguishes reused bracket ids after a setup reset', () => {
  const old = makeMatch({ games: [game(0, 10)] });
  const baseline = gameUpdates([old], 10, null).seen;
  const restarted = makeMatch({
    games: [{ ...game(0, 20), opened_at_ms: 10_000 }],
  });
  assert.equal(gameUpdates([restarted], 10, baseline).notifications.length, 1);
});
