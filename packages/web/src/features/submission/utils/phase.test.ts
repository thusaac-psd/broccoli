import assert from 'node:assert/strict';
import { test } from 'node:test';

import { submissionPhase } from './phase.ts';

const DISABLED = { state: 'disabled', plugin_id: 'icpc' } as const;

test('a submission waiting for its contest plugin is waiting, whatever its status says', () => {
  for (const status of ['Queued', 'Pending', 'Compiling', 'Running'] as const) {
    assert.equal(
      submissionPhase({ status, plugin_outage: DISABLED }),
      'waiting',
    );
    assert.equal(submissionPhase({ status }), 'judging');
  }
});

test('a decided or withheld submission is never in progress', () => {
  for (const status of ['Judged', 'CompilationError', 'SystemError'] as const) {
    assert.equal(submissionPhase({ status, plugin_outage: DISABLED }), 'final');
  }
  assert.equal(submissionPhase({ status: null }), 'withheld');
});
