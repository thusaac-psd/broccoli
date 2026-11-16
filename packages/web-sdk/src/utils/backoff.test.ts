import assert from 'node:assert/strict';
import { test } from 'node:test';

import { backoffDelay } from './backoff.ts';

const policy = { baseMs: 1_000, maxMs: 5_000 };

test('the delay doubles from the base up to the cap', () => {
  assert.deepEqual(
    [0, 1, 2, 3, 10].map((retry) => backoffDelay(retry, policy)),
    [1_000, 2_000, 4_000, 5_000, 5_000],
  );
});

test('jitter keeps the delay in the upper half of the window', () => {
  assert.equal(
    backoffDelay(2, policy, () => 0),
    2_000,
  );
  assert.equal(
    backoffDelay(2, policy, () => 1),
    4_000,
  );
});
