import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { MatchView } from '../types.ts';
import { matchSteps, playerStage, sideOf } from './stage.ts';

function match(overrides: Partial<MatchView>): MatchView {
  return {
    id: 0,
    round: 1,
    pos: 0,
    player_a: 10,
    player_b: 20,
    player_a_name: 'alice',
    player_b_name: 'bob',
    group_a: [101, 102, 103],
    group_b: [201, 202, 203],
    order_a: null,
    order_b: null,
    tiebreak_problem: null,
    score_a: 0,
    score_b: 0,
    state: 'ordering',
    winner: null,
    decided_at_ms: 0,
    awaiting_submission_id: null,
    current_xiaoju_index: null,
    current_xiaoju_deadline_ms: null,
    current_xiaoju_opened_at_ms: null,
    games: [],
    starts_at_ms: null,
    ...overrides,
  };
}

test('sideOf finds the viewer, or null for anyone else', () => {
  const m = match({});
  assert.equal(sideOf(m, 10), 'a');
  assert.equal(sideOf(m, 20), 'b');
  assert.equal(sideOf(m, 30), null);
  assert.equal(sideOf(m, null), null);
});

test("a player's own ranking lands in the OPPONENT's order", () => {
  // A ranked (order_b is set) but B has not (order_a is null).
  const m = match({ order_b: [203, 201, 202] });
  assert.deepEqual(playerStage(m, 'a'), {
    kind: 'waiting_start',
    startsAtMs: null,
  });
  assert.equal(playerStage(m, 'b').kind, 'rank');
});

test('during a game each player is pointed at their own problem', () => {
  const m = match({
    state: 'in_progress',
    order_a: [103, 101, 102],
    order_b: [203, 201, 202],
    current_xiaoju_index: 1,
  });
  assert.deepEqual(playerStage(m, 'a'), {
    kind: 'play',
    game: 1,
    problemId: 101,
  });
  assert.deepEqual(playerStage(m, 'b'), {
    kind: 'play',
    game: 1,
    problemId: 201,
  });
});

test('a tiebreak points both players at the shared problem', () => {
  const m = match({
    state: 'tiebreak',
    current_xiaoju_index: 3,
    tiebreak_problem: 301,
  });
  assert.deepEqual(playerStage(m, 'a'), {
    kind: 'play',
    game: 3,
    problemId: 301,
  });
  assert.deepEqual(playerStage(m, 'b'), {
    kind: 'play',
    game: 3,
    problemId: 301,
  });
});

test('a decided match tells each side whether they advanced', () => {
  const m = match({ state: 'decided', winner: 20 });
  assert.equal(playerStage(m, 'a').kind, 'eliminated');
  assert.equal(playerStage(m, 'b').kind, 'advanced');
});

test('waiting on the judge is its own stage, not "play"', () => {
  const m = match({ state: 'awaiting_judge', current_xiaoju_index: 0 });
  assert.equal(playerStage(m, 'a').kind, 'judging');
});

test('the stepper marks exactly one current step while a game runs', () => {
  const game = (index: number, decided: boolean) => ({
    index,
    tiebreak: false,
    problem_a: null,
    problem_b: null,
    opened_at_ms: 0,
    deadline_ms: 1,
    winner: null,
    decided,
  });
  const m = match({
    state: 'in_progress',
    order_a: [103, 101, 102],
    order_b: [203, 201, 202],
    current_xiaoju_index: 1,
    games: [game(0, true), game(1, false)],
  });
  const steps = matchSteps(m, 'a');
  assert.deepEqual(
    steps.map((s) => s.status),
    ['done', 'done', 'done', 'current', 'upcoming', 'upcoming'],
  );
  assert.equal(
    steps.some((s) => s.key === 'tiebreak'),
    false,
  );
});

test('the tiebreak step appears only once a tiebreak has opened', () => {
  const m = match({
    state: 'tiebreak',
    current_xiaoju_index: 3,
    tiebreak_problem: 301,
  });
  const keys = matchSteps(m, 'a').map((s) => s.key);
  assert.deepEqual(keys, [
    'rank',
    'start',
    'game',
    'game',
    'game',
    'tiebreak',
    'result',
  ]);
});

test('once both have ranked, the waiting player learns when the match starts', () => {
  const m = match({
    order_a: [103, 101, 102],
    order_b: [203, 201, 202],
    starts_at_ms: 90_000,
  });
  assert.deepEqual(playerStage(m, 'a'), {
    kind: 'waiting_start',
    startsAtMs: 90_000,
  });
});
