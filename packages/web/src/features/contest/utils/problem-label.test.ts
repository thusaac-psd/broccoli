import assert from 'node:assert/strict';
import { test } from 'node:test';

import { nextProblemLabel } from './problem-label.ts';

test('nextProblemLabel takes the first free single letter', () => {
  assert.equal(nextProblemLabel(new Set()), 'A');
  assert.equal(nextProblemLabel(new Set(['A', 'B', 'D'])), 'C');
});

test('nextProblemLabel moves on to two letters after Z', () => {
  const singles = new Set('ABCDEFGHIJKLMNOPQRSTUVWXYZ');
  assert.equal(nextProblemLabel(singles), 'AA');
  assert.equal(nextProblemLabel(new Set([...singles, 'AA', 'AB'])), 'AC');
});

test('nextProblemLabel is empty when every label is taken', () => {
  const letters = [...'ABCDEFGHIJKLMNOPQRSTUVWXYZ'];
  const all = new Set([
    ...letters,
    ...letters.flatMap((a) => letters.map((b) => a + b)),
  ]);
  assert.equal(nextProblemLabel(all), '');
});
