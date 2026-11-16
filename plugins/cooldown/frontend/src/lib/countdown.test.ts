import assert from 'node:assert/strict';
import { test } from 'node:test';

import { secondsLeft } from './countdown.ts';

test('the wait counts down from the moment the server reported it', () => {
  assert.equal(secondsLeft(30, 1_000, 1_000), 30);
  assert.equal(secondsLeft(30, 1_000, 1_001), 30);
  assert.equal(secondsLeft(30, 1_000, 2_000), 29);
  assert.equal(secondsLeft(30, 1_000, 30_999), 1);
  assert.equal(secondsLeft(30, 1_000, 31_000), 0);
});

test('a tick that fires late still reads the time actually left', () => {
  // A hidden tab ran one tick in the last 25 seconds.
  assert.equal(secondsLeft(30, 0, 25_000), 5);
  assert.equal(secondsLeft(30, 0, 90_000), 0);
});
