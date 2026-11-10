import assert from 'node:assert/strict';
import { test } from 'node:test';

import { createServerClock } from './server-clock.ts';

// A contestant PC 8 hours fast (a dual-boot RTC in local time, UTC+8), and a
// server whose clock reads `serverAt(local)` when the local clock reads
// `local`.
const SKEW_MS = 8 * 3600 * 1000;
const T0 = Date.parse('2026-11-10T01:00:00.000Z');
const serverAt = (local: number) => local - SKEW_MS;
// How a server renders its clock into a `Date` header: whole seconds.
const dateHeader = (serverMs: number) => new Date(serverMs).toUTCString();

function exchange(
  clock: ReturnType<typeof createServerClock>,
  sentAt: number,
  rttMs: number,
  stampAfterMs = rttMs / 2,
) {
  clock.observe(
    dateHeader(serverAt(sentAt + stampAfterMs)),
    sentAt,
    sentAt + rttMs,
  );
}

test('without a sample the local clock is used', () => {
  let local = T0;
  const clock = createServerClock(() => local);
  assert.equal(clock.now(), T0);
  clock.observe(null, local, local + 10);
  clock.observe('not a date', local, local + 10);
  local += 5;
  assert.equal(clock.now(), T0 + 5);
});

test('one response corrects a clock that is hours off to within a second', () => {
  const local = T0 + 123;
  const clock = createServerClock(() => local);
  exchange(clock, local - 40, 40);
  assert.ok(Math.abs(clock.now() - serverAt(local)) <= 1_000);
});

test('successive responses converge well below the header resolution', () => {
  let local = T0;
  const clock = createServerClock(() => local);
  // Responses at assorted sub-second phases, as an app's traffic produces.
  for (const phase of [0, 130, 270, 410, 560, 690, 830, 970, 50, 610]) {
    local += 7_000 + phase;
    exchange(clock, local - 20, 20);
  }
  assert.ok(Math.abs(clock.now() - serverAt(local)) <= 60);
});

test('a clock jump starts the estimate over', () => {
  let local = T0;
  const clock = createServerClock(() => local);
  exchange(clock, local - 20, 20);
  // The user fixes their clock: local time drops by the 8 hours.
  local += 10_000 - SKEW_MS;
  clock.observe(dateHeader(local - 10), local - 20, local);
  assert.ok(Math.abs(clock.now() - local) <= 1_000);
});

test('subscribers hear about real moves, not every refinement', () => {
  let local = T0;
  const clock = createServerClock(() => local);
  let calls = 0;
  const unsubscribe = clock.subscribe(() => {
    calls += 1;
  });
  exchange(clock, local - 20, 20);
  assert.equal(calls, 1);
  assert.ok(Math.abs(clock.publishedOffset() + SKEW_MS) <= 1_000);

  for (let i = 0; i < 10; i += 1) {
    local += 3_333;
    exchange(clock, local - 20, 20);
  }
  const settled = calls;
  assert.ok(settled <= 3);
  for (let i = 0; i < 10; i += 1) {
    local += 3_333;
    exchange(clock, local - 20, 20);
  }
  assert.equal(calls, settled);
  unsubscribe();
});
