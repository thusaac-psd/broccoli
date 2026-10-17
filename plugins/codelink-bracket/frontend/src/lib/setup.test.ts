import assert from 'node:assert/strict';
import { test } from 'node:test';

import { autoFillRounds, validateSetup } from './setup.ts';

const seeds = Array.from({ length: 16 }, (_, i) => i + 1);
const problems = Array.from({ length: 28 }, (_, i) => 100 + i);

test('shared extra time is independent of stage timings, penalties and judge grace', () => {
  const rounds = autoFillRounds(problems);
  rounds[0]!.gameMinutes = 10;
  rounds[1]!.gameMinutes = 20;
  for (const extra of [0, 5]) {
    assert.deepEqual(validateSetup(seeds, rounds, 7, 120, extra), []);
  }
  for (const extra of [-1, NaN, Infinity, Number.MAX_VALUE]) {
    assert.deepEqual(validateSetup(seeds, rounds, 7, 120, extra), [
      { kind: 'badExtra' },
    ]);
  }
  assert.deepEqual(validateSetup(seeds, rounds, -1, 120, 5), [
    { kind: 'badPenalty' },
  ]);
  assert.deepEqual(validateSetup(seeds, rounds, 7, -1, 5), [
    { kind: 'badGrace' },
  ]);
});

test('auto-fill assigns 3 + 3 + 1 problems per round in contest order', () => {
  const rounds = autoFillRounds(problems);
  assert.equal(rounds.length, 4);
  assert.deepEqual(rounds[0], {
    groupA: [100, 101, 102],
    groupB: [103, 104, 105],
    tiebreak: [106],
    gameMinutes: 30,
    tiebreakMinutes: 30,
    breakMinutes: 0,
  });
  assert.deepEqual(rounds[3]?.tiebreak, [127]);
});

test('a complete, distinct setup has no issues', () => {
  assert.deepEqual(validateSetup(seeds, autoFillRounds(problems), 20, 120), []);
});

test('too few problems leaves empty slots in the later rounds', () => {
  const issues = validateSetup(
    seeds,
    autoFillRounds(problems.slice(0, 20)),
    20,
    120,
  );
  assert.deepEqual(
    issues.map((i) => i.kind === 'emptySlot' && i.round).filter(Boolean),
    [3, 4],
  );
});

test('a problem used twice is reported once, like the server rejects it', () => {
  const rounds = autoFillRounds(problems);
  rounds[1]!.groupA[0] = 100;
  assert.deepEqual(validateSetup(seeds, rounds, 20, 120), [
    { kind: 'duplicate', problemId: 100 },
  ]);
});

test('the bracket needs exactly 16 distinct players', () => {
  const issues = validateSetup(
    [...seeds.slice(0, 15), 1],
    autoFillRounds(problems),
    20,
    120,
  );
  assert.deepEqual(issues, [{ kind: 'players', count: 15 }]);
});

test('each stage validates regular games, tiebreaks and its opening break', () => {
  for (const field of [
    'gameMinutes',
    'tiebreakMinutes',
    'breakMinutes',
  ] as const) {
    for (const value of [-1, NaN, Infinity, Number.MAX_VALUE]) {
      const rounds = autoFillRounds(problems);
      rounds[2]![field] = value;
      assert.deepEqual(validateSetup(seeds, rounds, 20, 120), [
        { kind: 'badTiming', round: 3 },
      ]);
    }
  }
  for (const field of ['gameMinutes', 'tiebreakMinutes'] as const) {
    const rounds = autoFillRounds(problems);
    rounds[1]![field] = 0;
    assert.deepEqual(validateSetup(seeds, rounds, 20, 120), [
      { kind: 'badTiming', round: 2 },
    ]);
  }
  const rounds = autoFillRounds(problems);
  rounds[0]!.breakMinutes = 1;
  assert.deepEqual(validateSetup(seeds, rounds, 20, 120), [
    { kind: 'badTiming', round: 1 },
  ]);
});

test('refilling problem slots preserves every stage timing choice', () => {
  const rounds = autoFillRounds(problems);
  rounds.forEach((r, i) => {
    r.gameMinutes = 10 + i * 10;
    r.tiebreakMinutes = 15 + i * 10;
    r.breakMinutes = i * 5;
  });
  const refilled = autoFillRounds([...problems].reverse(), rounds);
  refilled.forEach((r, i) => {
    assert.equal(r.gameMinutes, rounds[i]!.gameMinutes);
    assert.equal(r.tiebreakMinutes, rounds[i]!.tiebreakMinutes);
    assert.equal(r.breakMinutes, rounds[i]!.breakMinutes);
  });
  assert.deepEqual(refilled[0]!.groupA, [127, 126, 125]);
  assert.deepEqual(validateSetup(seeds, refilled, 20, 120), []);
});

test('penalties accept zero and custom whole seconds and reject invalid values', () => {
  const rounds = autoFillRounds(problems);
  for (const seconds of [0, 7, 30]) {
    assert.deepEqual(validateSetup(seeds, rounds, seconds, 120), []);
  }
  for (const seconds of [-1, 1.5, NaN, Infinity]) {
    assert.deepEqual(validateSetup(seeds, rounds, seconds, 120), [
      { kind: 'badPenalty' },
    ]);
  }
});

test('judge wait accepts zero and whole seconds and rejects invalid values', () => {
  const rounds = autoFillRounds(problems);
  for (const seconds of [0, 7, 120]) {
    assert.deepEqual(validateSetup(seeds, rounds, 20, seconds), []);
  }
  for (const seconds of [-1, 1.5, NaN, Infinity]) {
    assert.deepEqual(validateSetup(seeds, rounds, 20, seconds), [
      { kind: 'badGrace' },
    ]);
  }
});

test('eight-player setup uses three stages and can assign five games per stage', () => {
  const initial = autoFillRounds(problems, undefined, 3);
  initial[1].groupA = [null, null, null, null, null];
  initial[1].groupB = [null, null, null, null, null];
  const filled = autoFillRounds(
    Array.from({ length: 40 }, (_, i) => i + 1),
    initial,
    3,
  );
  assert.equal(filled.length, 3);
  assert.equal(filled[1].groupA.length, 5);
  assert.deepEqual(validateSetup(seeds.slice(0, 8), filled, 20, 120, 0, 8), []);
});

test('auto-fill preserves opened stages and excludes their problems from future stages', () => {
  const before = autoFillRounds(problems);
  const after = autoFillRounds([...problems].reverse(), before, 4, 1);
  assert.deepEqual(after[0], before[0]);
  assert.deepEqual(validateSetup(seeds, after, 20, 120), []);
  assert.notDeepEqual(after[1], before[1]);
});
