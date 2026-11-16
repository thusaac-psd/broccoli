import assert from 'node:assert/strict';
import { test } from 'node:test';

import { contestStatus, phaseHasEnded, phaseHasStarted } from './status.ts';

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
