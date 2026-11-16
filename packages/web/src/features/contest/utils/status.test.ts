import assert from 'node:assert/strict';
import { test } from 'node:test';

import { PLUGIN_OUTAGE_POLL_MS } from '@broccoli/web-sdk/hooks';

import {
  contestPollPace,
  contestStatus,
  nextPhaseChange,
  PHASE_DUE_POLL_MS,
  phaseHasEnded,
  phaseHasStarted,
} from './status.ts';

const times = {
  start_time: '2026-01-02T00:00:00Z',
  end_time: '2026-01-02T05:00:00Z',
};

test('an unactivated contest is a draft without an activate time', () => {
  assert.equal(
    contestStatus({ phase: 'unactivated', activate_time: null }),
    'draft',
  );
  assert.equal(
    contestStatus({
      phase: 'unactivated',
      activate_time: '2026-01-01T00:00:00Z',
    }),
    'scheduled',
  );
});

test('every other status is the server phase as it is', () => {
  for (const phase of [
    'upcoming',
    'running',
    'ended',
    'deactivated',
  ] as const) {
    assert.equal(
      contestStatus({ phase, activate_time: '2026-01-01T00:00:00Z' }),
      phase,
    );
  }
});

test('the phase changes next at the start, then at the end', () => {
  assert.equal(
    nextPhaseChange({ phase: 'upcoming', ...times }),
    Date.parse(times.start_time),
  );
  assert.equal(
    nextPhaseChange({ phase: 'running', ...times }),
    Date.parse(times.end_time),
  );
  assert.equal(nextPhaseChange({ phase: 'ended', ...times }), null);
  assert.equal(nextPhaseChange({ phase: 'unactivated', ...times }), null);
});

test('a contest has started once it runs', () => {
  assert.equal(phaseHasStarted('upcoming'), false);
  assert.equal(phaseHasStarted('unactivated'), false);
  assert.equal(phaseHasStarted('running'), true);
  assert.equal(phaseHasStarted('ended'), true);
});

test('a contest has ended once the server says so, not once our clock passes its end', () => {
  assert.equal(phaseHasEnded('running'), false);
  assert.equal(phaseHasEnded('upcoming'), false);
  assert.equal(phaseHasEnded('ended'), true);
  assert.equal(phaseHasEnded('deactivated'), true);
});

const start = Date.parse(times.start_time);
const end = Date.parse(times.end_time);

test("a contest is asked for again once its phase is due, at this seat's offset", () => {
  const upcoming = { phase: 'upcoming', ...times } as const;
  assert.equal(contestPollPace(upcoming, start - 10_000, 700), 10_700);
  assert.equal(contestPollPace(upcoming, start - 10, 700), 710);
  const running = { phase: 'running', ...times } as const;
  assert.equal(contestPollPace(running, end - 5_000, 0), 5_000);
});

test('a contest past its due phase change is asked at a steady pace, not reset by the clock', () => {
  const upcoming = { phase: 'upcoming', ...times } as const;
  // Every render asks again; the answer stays the same, so TanStack keeps
  // its timer instead of restarting it.
  assert.equal(contestPollPace(upcoming, start + 1, 0), PHASE_DUE_POLL_MS);
  assert.equal(contestPollPace(upcoming, start + 900, 0), PHASE_DUE_POLL_MS);
});

test('a contest whose plugin is out is asked whether it is back, slowly', () => {
  const ended = {
    phase: 'ended',
    ...times,
    plugin_outage: { state: 'disabled', plugin_id: 'ioi' },
  } as const;
  assert.equal(contestPollPace(ended, end + 1, 0), PLUGIN_OUTAGE_POLL_MS);
  const running = { ...ended, phase: 'running' } as const;
  assert.equal(contestPollPace(running, end - 1_000, 0), 1_000);
});

test('a contest with nothing due and no outage is not polled', () => {
  assert.equal(
    contestPollPace({ phase: 'ended', ...times }, end + 1, 0),
    false,
  );
  assert.equal(
    contestPollPace({ phase: 'unactivated', ...times }, start - 1, 0),
    false,
  );
});

test('a phase change too far off for a timer waits as long as a timer can', () => {
  const upcoming = { phase: 'upcoming', ...times } as const;
  assert.equal(contestPollPace(upcoming, start - 2 ** 40, 0), 2 ** 31 - 1);
});
