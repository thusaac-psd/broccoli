import assert from 'node:assert/strict';
import { test } from 'node:test';

import { RequestError } from '@broccoli/web-sdk/api';

import { queryRetryDelay } from './query-client.ts';

const busy = (retryAfterMs: number | null) =>
  new RequestError(503, 'QUEUE_OVERLOADED', '', undefined, retryAfterMs);

test("a retry waits at least the server's Retry-After", () => {
  for (const random of [() => 0, () => 0.5, () => 1]) {
    for (let attempt = 0; attempt < 6; attempt += 1) {
      assert.ok(queryRetryDelay(attempt, busy(5_000), random) >= 5_000);
    }
  }
});

test('seats that failed together retry at different instants', () => {
  for (const error of [busy(2_000), busy(null), RequestError.network()]) {
    for (let attempt = 0; attempt < 6; attempt += 1) {
      const early = queryRetryDelay(attempt, error, () => 0);
      const late = queryRetryDelay(attempt, error, () => 1);
      assert.ok(early < late, `${error.code} retry ${attempt}`);
    }
  }
});

test('each retry waits longer, up to the poll backoff ceiling', () => {
  const waits = Array.from({ length: 8 }, (_, attempt) =>
    queryRetryDelay(attempt, busy(null), () => 1),
  );
  assert.deepEqual(
    waits,
    [2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000, 30_000],
  );
});
