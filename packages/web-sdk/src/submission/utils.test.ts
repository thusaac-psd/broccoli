import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { isTerminalStatus, withheldLabel } from './utils.ts';

const t = ((key: string) => key) as Parameters<typeof withheldLabel>[1];

describe('a withheld status', () => {
  it('is final for this viewer, so pollers stop', () => {
    assert.equal(isTerminalStatus(null), true);
    assert.equal(isTerminalStatus(undefined), true);
    assert.equal(isTerminalStatus('Running'), false);
    assert.equal(isTerminalStatus('Judged'), true);
  });

  it('says why it is withheld, never that it is running', () => {
    assert.equal(
      withheldLabel({ reason: 'hidden' }, t),
      'submission.resultHidden',
    );
    assert.equal(
      withheldLabel(
        {
          reason: 'plugin_outage',
          outage: { state: 'disabled', plugin_id: 'ioi' },
        },
        t,
      ),
      'submission.resultUnavailableDisabled',
    );
    assert.equal(
      withheldLabel(
        { reason: 'plugin_outage', outage: { state: 'unavailable' } },
        t,
      ),
      'submission.resultUnavailable',
    );
  });
});
