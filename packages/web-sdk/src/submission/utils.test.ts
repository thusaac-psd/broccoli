import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { isTerminalStatus, withheldLabel } from './utils.ts';

const t = ((key: string) => key) as Parameters<typeof withheldLabel>[1];

describe('a status', () => {
  it('is terminal only once it can never change again', () => {
    assert.equal(isTerminalStatus('Running'), false);
    assert.equal(isTerminalStatus('Judged'), true);
  });
});

describe('a withheld status', () => {
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
        {
          reason: 'plugin_outage',
          outage: { state: 'not_loaded', plugin_id: 'ioi' },
        },
        t,
      ),
      'submission.resultUnavailableNotLoaded',
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
