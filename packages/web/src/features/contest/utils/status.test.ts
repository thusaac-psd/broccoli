import assert from 'node:assert/strict';
import { test } from 'node:test';

import { contestStatus, nextPhaseChange, phaseHasStarted } from './status.ts';

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
