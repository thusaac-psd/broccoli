// Pure-logic test for `deriveCountdownStatus` (see `countdown.ts`),
// following the `node --experimental-strip-types --test` convention.
//
// The invariant this suite exists to pin: the local countdown reaching (or
// passing) zero must NEVER be reported the same way as a server-confirmed
// decision. "Getting this backwards produces a UI that contradicts the
// official outcome" (task spec) -- these tests exercise exactly the
// boundary where that backwards behaviour would show up: `remainingMs`
// crossing zero while `decided` is still `false`.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { deriveCountdownStatus } from './countdown.ts';

test('no timing data yields "no-data", not a silent zero countdown', () => {
  assert.deepEqual(deriveCountdownStatus(null, 1_000), { kind: 'no-data' });
});

test('time remaining before the deadline yields a live countdown', () => {
  const status = deriveCountdownStatus(
    { deadlineMs: 10_000, decided: false },
    3_000,
  );
  assert.deepEqual(status, { kind: 'counting', remainingMs: 7_000 });
});

test('the exact deadline instant, undecided, is a waiting state (not counting, not decided)', () => {
  // Boundary case: `remainingMs === 0`. Must not be reported as "counting"
  // (there is nothing left to count down) and must not be reported as
  // "decided" (the server has not said so) -- only "waiting-for-server" is
  // honest here.
  const status = deriveCountdownStatus(
    { deadlineMs: 10_000, decided: false },
    10_000,
  );
  assert.deepEqual(status, { kind: 'waiting-for-server', overdueMs: 0 });
});

test('past the deadline, undecided, is a waiting state -- never a fabricated result', () => {
  // THE central regression guard: this is the exact scenario the spec calls
  // out -- "if the countdown reaches zero before the server has decided,
  // show a waiting state rather than announcing a result the server has not
  // made". An implementation that treated "time's up" as "some result now
  // exists" would fail this test.
  const status = deriveCountdownStatus(
    { deadlineMs: 10_000, decided: false },
    12_500,
  );
  assert.deepEqual(status, { kind: 'waiting-for-server', overdueMs: 2_500 });
});

test('once the server has decided, the status is "decided" regardless of remaining time', () => {
  // A 小局 can be decided BEFORE its deadline (e.g. both players' outcomes
  // are already known) -- `decided: true` must win even while `nowMs` is
  // still well before `deadlineMs`, not be overridden by a "still counting"
  // read of the clock.
  const status = deriveCountdownStatus(
    { deadlineMs: 10_000, decided: true },
    3_000,
  );
  assert.deepEqual(status, { kind: 'decided' });
});

test('decided also wins past the deadline (the ordinary case)', () => {
  const status = deriveCountdownStatus(
    { deadlineMs: 10_000, decided: true },
    15_000,
  );
  assert.deepEqual(status, { kind: 'decided' });
});
