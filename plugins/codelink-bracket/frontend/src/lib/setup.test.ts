import assert from 'node:assert/strict';
import { test } from 'node:test';

import { autoFillRounds, validateSetup } from './setup.ts';

const seeds = Array.from({ length: 16 }, (_, i) => i + 1);
const problems = Array.from({ length: 28 }, (_, i) => 100 + i);

test('auto-fill assigns 3 + 3 + 1 problems per round in contest order', () => {
  const rounds = autoFillRounds(problems);
  assert.equal(rounds.length, 4);
  assert.deepEqual(rounds[0], {
    groupA: [100, 101, 102],
    groupB: [103, 104, 105],
    tiebreak: [106],
  });
  assert.deepEqual(rounds[3]?.tiebreak, [127]);
});

test('a complete, distinct setup has no issues', () => {
  assert.deepEqual(validateSetup(seeds, autoFillRounds(problems), 30), []);
});

test('too few problems leaves empty slots in the later rounds', () => {
  const issues = validateSetup(
    seeds,
    autoFillRounds(problems.slice(0, 20)),
    30,
  );
  assert.deepEqual(
    issues.map((i) => i.kind === 'emptySlot' && i.round).filter(Boolean),
    [3, 4],
  );
});

test('a problem used twice is reported once, like the server rejects it', () => {
  const rounds = autoFillRounds(problems);
  rounds[1]!.groupA[0] = 100;
  assert.deepEqual(validateSetup(seeds, rounds, 30), [
    { kind: 'duplicate', problemId: 100 },
  ]);
});

test('the bracket needs exactly 16 distinct players', () => {
  const issues = validateSetup(
    [...seeds.slice(0, 15), 1],
    autoFillRounds(problems),
    30,
  );
  assert.deepEqual(issues, [{ kind: 'players', count: 15 }]);
});

test('a game must last longer than zero minutes', () => {
  assert.deepEqual(validateSetup(seeds, autoFillRounds(problems), 0), [
    { kind: 'badTiming' },
  ]);
});
