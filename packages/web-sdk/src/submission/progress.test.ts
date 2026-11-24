import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { PLUGIN_OUTAGE_POLL_MS } from '../hooks/poll.ts';
import {
  JUDGING_POLL_MS,
  submissionPhase,
  submissionPollPace,
} from './progress.ts';

const DISABLED = { state: 'disabled', plugin_id: 'icpc' } as const;
const PARKED = { plugin_id: 'icpc', provides: 'contest_type' } as const;
const UNAVAILABLE = { state: 'unavailable' } as const;

describe('where a submission stands', () => {
  it('is waiting, whatever its status says, while its contest plugin is out', () => {
    for (const status of [
      'Queued',
      'Pending',
      'Compiling',
      'Running',
    ] as const) {
      assert.equal(submissionPhase({ status, parked_by: PARKED }), 'waiting');
      assert.equal(submissionPhase({ status }), 'judging');
    }
  });

  it('is decided once its status is final, outage or not', () => {
    for (const status of [
      'Judged',
      'CompilationError',
      'SystemError',
    ] as const) {
      assert.equal(submissionPhase({ status, parked_by: PARKED }), 'final');
    }
  });

  it('is waiting while a plugin outage withholds its outcome', () => {
    for (const outage of [DISABLED, UNAVAILABLE]) {
      assert.equal(
        submissionPhase({
          status: null,
          withheld: { reason: 'plugin_outage', outage },
        }),
        'waiting',
      );
    }
  });

  it('is hidden while the contest hides its outcome', () => {
    assert.equal(
      submissionPhase({ status: null, withheld: { reason: 'hidden' } }),
      'hidden',
    );
    assert.equal(submissionPhase({ status: null }), 'hidden');
  });
});

describe('how often a submission is asked for', () => {
  it('polls fast while judged, slowly while waiting, and stops once nothing can change', () => {
    assert.equal(submissionPollPace({ status: 'Running' }), JUDGING_POLL_MS);
    assert.equal(
      submissionPollPace({ status: 'Queued', parked_by: PARKED }),
      PLUGIN_OUTAGE_POLL_MS,
    );
    assert.equal(
      submissionPollPace({
        status: null,
        withheld: { reason: 'plugin_outage', outage: UNAVAILABLE },
      }),
      PLUGIN_OUTAGE_POLL_MS,
    );
    assert.equal(
      submissionPollPace({ status: null, withheld: { reason: 'hidden' } }),
      false,
    );
    assert.equal(submissionPollPace({ status: 'Judged' }), false);
  });
});
