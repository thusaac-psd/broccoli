// Pure-logic test for `diffNullableField` / `testCaseDiffStatus` (see
// `testCaseDiff.ts`). Follows the same convention as `verdict-key.test.ts`:
// the repo has no frontend test runner configured (`pnpm test` at the root
// is a no-op placeholder) and none is introduced by this change; this file
// runs standalone via Node's built-in test runner + native TypeScript
// support (`node --experimental-strip-types --test testCaseDiff.test.ts`,
// Node >= 22), since the functions under test are pure and have no
// React/JSX involved.
//
// This pins the fix for the finding: `TestCaseResultResponse.verdict`/
// `.score` are `Option`-widened, and under visibility masking BOTH the
// historical and current test case's fields read as `null` for the same
// viewer. A naive `!==` comparison silently reports "unchanged" for a field
// it cannot actually observe, so the "verdict/score changed on rejudge"
// badge could never fire for a masked test case. The fix must distinguish
// "unknown" (both sides masked) from "unchanged" (both sides observed and
// equal) -- and must never collapse "unknown" into "unchanged".
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { diffNullableField, testCaseDiffStatus } from './testCaseDiff.ts';

test('diffNullableField: both sides masked (null) is unknown, not same', () => {
  assert.equal(diffNullableField(null, null), 'unknown');
});

test('diffNullableField: both sides masked (undefined) is unknown, not same', () => {
  assert.equal(diffNullableField(undefined, undefined), 'unknown');
});

test('diffNullableField: both sides observed and equal is same', () => {
  assert.equal(diffNullableField('Accepted', 'Accepted'), 'same');
  assert.equal(diffNullableField(10, 10), 'same');
});

test('diffNullableField: both sides observed and unequal is different', () => {
  assert.equal(diffNullableField('Accepted', 'WrongAnswer'), 'different');
  assert.equal(diffNullableField(10, 5), 'different');
});

test('diffNullableField: exactly one side masked is different, never same', () => {
  assert.equal(diffNullableField(null, 'Accepted'), 'different');
  assert.equal(diffNullableField('Accepted', null), 'different');
  assert.equal(diffNullableField(undefined, 10), 'different');
  assert.equal(diffNullableField(10, undefined), 'different');
});

test('testCaseDiffStatus: fully masked test case on both sides is unknown, not unchanged', () => {
  // This is the exact regression scenario: a frozen/restricted test case
  // whose verdict and score are both null on the historical AND the
  // current judgement. The old `!==`-based comparison reported this as
  // "unchanged" (no badge); the badge must instead surface that a diff
  // cannot be verified, rather than silently claiming nothing changed.
  const masked = {
    verdict: null,
    score: null,
    time_used: null,
    memory_used: null,
    checker_output: null,
  };
  assert.equal(testCaseDiffStatus(masked, { ...masked }), 'unknown');
});

test('testCaseDiffStatus: identical observed fields is unchanged', () => {
  const testCase = {
    verdict: 'Accepted',
    score: 100,
    time_used: 50,
    memory_used: 1024,
    checker_output: 'exact match',
  };
  assert.equal(testCaseDiffStatus(testCase, { ...testCase }), 'unchanged');
});

test('testCaseDiffStatus: an observably different field is changed', () => {
  const before = {
    verdict: 'WrongAnswer',
    score: 0,
    time_used: 50,
    memory_used: 1024,
    checker_output: null,
  };
  const after = { ...before, verdict: 'Accepted', score: 100 };
  assert.equal(testCaseDiffStatus(before, after), 'changed');
});

test('testCaseDiffStatus: a real change on an unmasked field outranks an unrelated masked field', () => {
  const before = {
    verdict: 'WrongAnswer',
    score: 0,
    time_used: null,
    memory_used: null,
    checker_output: null,
  };
  const after = {
    verdict: 'Accepted',
    score: 100,
    time_used: null,
    memory_used: null,
    checker_output: null,
  };
  assert.equal(testCaseDiffStatus(before, after), 'changed');
});

test('testCaseDiffStatus: one field masked on only one side, rest identical, is changed', () => {
  const before = {
    verdict: 'Accepted',
    score: 100,
    time_used: 50,
    memory_used: 1024,
    checker_output: null,
  };
  const after = { ...before, checker_output: 'ok' };
  // checker_output is observable on `after` but masked on `before`, and the
  // rest are identical. Per `diffNullableField`, exactly-one-side-masked is
  // `different` -- reachable in principle if masking policy changes
  // mid-history, and still correctly refuses to claim "unchanged".
  assert.equal(testCaseDiffStatus(before, after), 'changed');
});
