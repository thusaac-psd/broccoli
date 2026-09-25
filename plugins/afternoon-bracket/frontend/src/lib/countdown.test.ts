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

import type { MatchPhase } from '../types.ts';
import {
  deriveCountdownStatus,
  formatCountdownLabel,
  formatDurationMs,
  xiaojuTimingFromMatch,
} from './countdown.ts';

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

// -- xiaojuTimingFromMatch: turning a real MatchView into XiaojuTiming --

test('xiaojuTimingFromMatch returns null (no clock) when the deadline field is absent', () => {
  assert.equal(
    xiaojuTimingFromMatch({
      current_xiaoju_deadline_ms: null,
      state: 'pending',
    }),
    null,
  );
});

test('xiaojuTimingFromMatch carries the real deadline through for an in-progress match', () => {
  const timing = xiaojuTimingFromMatch({
    current_xiaoju_deadline_ms: 65_000,
    state: 'in_progress',
  });
  assert.deepEqual(timing, { deadlineMs: 65_000, decided: false });
});

test('xiaojuTimingFromMatch marks decided=true once the MATCH is decided, even with a deadline present', () => {
  // THE regression case: the server does not clear `xiaoju` on decision (see
  // `apply_force_decide`/the natural win path in judge.rs), so a decided
  // match can still carry a stale, present `current_xiaoju_deadline_ms`.
  // `decided` must come from `state`, not from field-presence, or a decided
  // match would be reported as still counting down / waiting for the
  // server.
  const timing = xiaojuTimingFromMatch({
    current_xiaoju_deadline_ms: 65_000,
    state: 'decided',
  });
  assert.deepEqual(timing, { deadlineMs: 65_000, decided: true });
});

test('xiaojuTimingFromMatch marks decided=true for needs_adjudication too', () => {
  const timing = xiaojuTimingFromMatch({
    current_xiaoju_deadline_ms: 65_000,
    state: 'needs_adjudication',
  });
  assert.deepEqual(timing, { deadlineMs: 65_000, decided: true });
});

test('xiaojuTimingFromMatch feeding into deriveCountdownStatus reproduces AwaitingJudge as waiting-for-server with no special case', () => {
  // AwaitingJudge freezes the blocked 小局's (already-past) deadline and
  // leaves `state` short of `decided`/`needs_adjudication` -- the existing
  // deadline-math branch of `deriveCountdownStatus` should classify this as
  // "waiting for the server" on its own, with no AwaitingJudge-specific
  // branch anywhere in this module.
  const timing = xiaojuTimingFromMatch({
    current_xiaoju_deadline_ms: 10_000,
    state: 'awaiting_judge',
  });
  const status = deriveCountdownStatus(timing, 12_500);
  assert.deepEqual(status, { kind: 'waiting-for-server', overdueMs: 2_500 });
});

test('xiaojuTimingFromMatch: every MatchPhase is handled without throwing', () => {
  const allPhases: MatchPhase[] = [
    'pending',
    'ordering',
    'in_progress',
    'tiebreak',
    'awaiting_judge',
    'decided',
    'needs_adjudication',
  ];
  for (const state of allPhases) {
    assert.doesNotThrow(() =>
      xiaojuTimingFromMatch({ current_xiaoju_deadline_ms: 1_000, state }),
    );
  }
});

// -- formatDurationMs --

test('formatDurationMs formats sub-minute durations as 0:SS', () => {
  assert.equal(formatDurationMs(5_000), '0:05');
});

test('formatDurationMs formats multi-minute durations as M:SS', () => {
  assert.equal(formatDurationMs(65_000), '1:05');
});

test('formatDurationMs rounds to the nearest second rather than truncating', () => {
  assert.equal(formatDurationMs(59_600), '1:00');
});

test('formatDurationMs clamps a negative input to 0:00 rather than producing a negative-looking string', () => {
  assert.equal(formatDurationMs(-500), '0:00');
});

// -- formatCountdownLabel: exhaustive over every CountdownStatus kind --

// Stand-in translator: echoes the key and its params, so these tests pin
// which message is chosen and what gets interpolated, independent of locale.
const echoT = (key: string, params?: Record<string, string | number>) =>
  params ? `${key} ${JSON.stringify(params)}` : key;

test('formatCountdownLabel produces a non-empty, distinct label for every CountdownStatus kind', () => {
  const statuses: Parameters<typeof formatCountdownLabel>[0][] = [
    { kind: 'no-data' },
    { kind: 'counting', remainingMs: 65_000 },
    { kind: 'waiting-for-server', overdueMs: 2_500 },
    { kind: 'decided' },
  ];
  const labels = statuses.map((s) => formatCountdownLabel(s, echoT));
  for (const label of labels) {
    assert.ok(label.length > 0);
  }
  assert.equal(new Set(labels).size, labels.length, 'labels must be distinct');
});

test('formatCountdownLabel embeds the formatted remaining time for "counting"', () => {
  assert.match(
    formatCountdownLabel({ kind: 'counting', remainingMs: 65_000 }, echoT),
    /1:05/,
  );
});

test('formatCountdownLabel embeds the formatted overdue time for "waiting-for-server"', () => {
  assert.match(
    formatCountdownLabel(
      { kind: 'waiting-for-server', overdueMs: 2_500 },
      echoT,
    ),
    /0:03/,
  );
});
