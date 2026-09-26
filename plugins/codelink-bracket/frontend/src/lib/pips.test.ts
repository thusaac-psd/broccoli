import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { GameView, MatchView } from '../types.ts';
import { makeMatch } from './fixtures.test-util.ts';
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

const match = (overrides: Partial<MatchView>): MatchView =>
  makeMatch({ state: 'in_progress', ...overrides });

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

test('a game left open when staff decided the match is not shown as live', () => {
  const m = match({
    state: 'decided',
    winner: 10,
    games: [game(0, null), game(1, null, false)],
  });
  assert.deepEqual(gamePips(m, 10), ['void', 'void']);
  assert.deepEqual(gamePips(m, 20), ['void', 'void']);
});
