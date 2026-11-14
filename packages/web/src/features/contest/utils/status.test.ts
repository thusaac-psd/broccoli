import assert from 'node:assert/strict';
import { test } from 'node:test';

import { contestStatus } from './status.ts';

const at = (iso: string) => Date.parse(iso);
const window = {
  activate_time: '2026-01-01T00:00:00Z',
  start_time: '2026-01-02T00:00:00Z',
  end_time: '2026-01-02T05:00:00Z',
  deactivate_time: '2026-01-03T00:00:00Z',
};

test('a contest without an activate time is a draft, whatever its times', () => {
  const draft = { ...window, activate_time: null };
  assert.equal(contestStatus(draft, at('2026-01-02T01:00:00Z')), 'draft');
  assert.equal(contestStatus(draft, at('2027-01-01T00:00:00Z')), 'draft');
});

test('contestStatus follows the activation window around the phases', () => {
  assert.equal(contestStatus(window, at('2025-12-31T00:00:00Z')), 'scheduled');
  assert.equal(contestStatus(window, at('2026-01-01T12:00:00Z')), 'upcoming');
  assert.equal(contestStatus(window, at('2026-01-02T01:00:00Z')), 'running');
  assert.equal(contestStatus(window, at('2026-01-02T06:00:00Z')), 'ended');
  assert.equal(
    contestStatus(window, at('2026-01-03T00:00:00Z')),
    'deactivated',
  );
  const open = { ...window, deactivate_time: null };
  assert.equal(contestStatus(open, at('2027-01-01T00:00:00Z')), 'ended');
});
