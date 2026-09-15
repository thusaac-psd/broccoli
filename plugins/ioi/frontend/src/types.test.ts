// Pure-logic test for `normalizeMaskedTestCase` (see `types.ts`). The repo
// has no frontend test runner configured (`pnpm test` at the root is a
// no-op placeholder) and none is introduced by this change; this file runs
// standalone via Node's built-in test runner + native TypeScript support
// (`node --test src/types.test.ts`, Node >= 22), with no new dependency and
// no bundler/DOM needed, since the function under test is a pure object
// transform with no React/JSX involved.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { MaskedTestCaseResult } from './types.ts';
import { normalizeMaskedTestCase } from './types.ts';

function maskedTestCase(
  overrides: Partial<MaskedTestCaseResult>,
): MaskedTestCaseResult {
  return {
    id: 1,
    test_case_id: 1,
    verdict: 'Accepted',
    score: 100,
    time_used: 12,
    memory_used: 1024,
    ...overrides,
  } as MaskedTestCaseResult;
}

test('a null verdict (subtask_scores/total_only masking) normalizes to the "Skipped" label', () => {
  const tc = maskedTestCase({ verdict: null });
  const normalized = normalizeMaskedTestCase(tc);
  assert.equal(normalized.verdict, 'Skipped');
});

test('a null score (subtask_scores/total_only masking) normalizes to 0', () => {
  const tc = maskedTestCase({ score: null });
  const normalized = normalizeMaskedTestCase(tc);
  assert.equal(normalized.score, 0);
});

test('an unmasked (non-null) verdict and score pass through unchanged', () => {
  const tc = maskedTestCase({ verdict: 'WrongAnswer', score: 42 });
  const normalized = normalizeMaskedTestCase(tc);
  assert.equal(normalized.verdict, 'WrongAnswer');
  assert.equal(normalized.score, 42);
});

test('a genuine (unmasked) "Skipped" verdict -- e.g. a group_min short-circuit -- is left as "Skipped", not conflated with masking', () => {
  const tc = maskedTestCase({ verdict: 'Skipped', score: 0 });
  const normalized = normalizeMaskedTestCase(tc);
  assert.equal(normalized.verdict, 'Skipped');
  assert.equal(normalized.score, 0);
});

test('other fields are preserved untouched', () => {
  const tc = maskedTestCase({
    verdict: null,
    score: null,
    time_used: null,
    memory_used: null,
  });
  const normalized = normalizeMaskedTestCase(tc);
  assert.equal(normalized.id, 1);
  assert.equal(normalized.test_case_id, 1);
  assert.equal(normalized.time_used, null);
  assert.equal(normalized.memory_used, null);
});
