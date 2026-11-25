import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { isTerminalStatus, withheldLabel } from './utils.ts';

const i18n: Parameters<typeof withheldLabel>[1] = {
  t: ((key: string, params?: Record<string, string>) =>
    params ? `${key} ${JSON.stringify(params)}` : key) as Parameters<
    typeof withheldLabel
  >[1]['t'],
  lookup: (key: string) => (key === 'plugin.ioi.name' ? 'IOI' : undefined),
};

describe('a status', () => {
  it('is terminal only once it can never change again', () => {
    assert.equal(isTerminalStatus('Running'), false);
    assert.equal(isTerminalStatus('Judged'), true);
  });
});

describe('a withheld status', () => {
  it('says why it is withheld, never that it is running', () => {
    assert.equal(
      withheldLabel({ reason: 'hidden' }, i18n),
      'submission.resultHidden',
    );
    assert.equal(
      withheldLabel(
        {
          reason: 'plugin_outage',
          outage: { state: 'disabled', plugin_id: 'ioi' },
        },
        i18n,
      ),
      // The disabled plugin is named, by its pack's name for it.
      'submission.resultUnavailableDisabled {"plugin":"IOI"}',
    );
    assert.equal(
      withheldLabel(
        {
          reason: 'plugin_outage',
          outage: { state: 'not_loaded', plugin_id: 'ioi' },
        },
        i18n,
      ),
      'submission.resultUnavailableNotLoaded {"plugin":"IOI"}',
    );
    assert.equal(
      withheldLabel(
        { reason: 'plugin_outage', outage: { state: 'unavailable' } },
        i18n,
      ),
      'submission.resultUnavailable',
    );
  });
});
