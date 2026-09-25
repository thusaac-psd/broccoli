import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { GameView, MatchView } from '../types.ts';
import { gamePips, seedsFrom } from './pips.ts';

const game = (
  index: number,
  winner: number | null,
  decided = true,
): GameView => ({
  index,
  tiebreak: index >= 3,
  problem_a: null,
  problem_b: null,
  opened_at_ms: 0,
  deadline_ms: 0,
  winner,
  decided,
});

function match(overrides: Partial<MatchView>): MatchView {
  return {
    id: 0,
    round: 1,
    pos: 0,
    player_a: 10,
    player_b: 20,
    player_a_name: null,
    player_b_name: null,
    group_a: [null, null, null],
    group_b: [null, null, null],
    order_a: null,
    order_b: null,
    tiebreak_problem: null,
    score_a: 0,
    score_b: 0,
    state: 'in_progress',
    winner: null,
    decided_at_ms: 0,
    awaiting_submission_id: null,
    current_xiaoju_index: null,
    current_xiaoju_deadline_ms: null,
    current_xiaoju_opened_at_ms: null,
    games: [],
    ...overrides,
  };
}

test('a running match shows results, the live game, then what is left', () => {
  const m = match({ games: [game(0, 10), game(1, null, false)] });
  assert.deepEqual(gamePips(m, 10), ['won', 'live', 'upcoming']);
  assert.deepEqual(gamePips(m, 20), ['lost', 'live', 'upcoming']);
});

test('a scoreless game is void for both players', () => {
  const m = match({ games: [game(0, null)] });
  assert.equal(gamePips(m, 10)[0], 'void');
  assert.equal(gamePips(m, 20)[0], 'void');
});

test('tiebreaks extend the row, and a decided match shows no upcoming pips', () => {
  const m = match({
    state: 'decided',
    games: [
      game(0, 10),
      game(1, 20),
      game(2, null),
      game(3, null),
      game(4, 20),
    ],
  });
  assert.deepEqual(gamePips(m, 20), ['lost', 'won', 'void', 'void', 'won']);
});

test('seeds come from first-round positions', () => {
  const seeds = seedsFrom([
    match({ pos: 0, player_a: 1, player_b: 2 }),
    match({ pos: 3, player_a: 7, player_b: 8 }),
    match({ round: 2, pos: 0, player_a: 1, player_b: 7 }),
  ]);
  assert.deepEqual(
    [...seeds.entries()],
    [
      [1, 1],
      [2, 2],
      [7, 7],
      [8, 8],
    ],
  );
});
