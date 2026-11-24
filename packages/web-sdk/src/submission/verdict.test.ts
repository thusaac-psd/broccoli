import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { I18nContextValue } from '@/i18n';

import { SUBMISSION_STATUS_FILTER_OPTIONS } from './utils.ts';
import { getVerdictBadge, progressLabel } from './verdict.ts';

const t = ((key: string) => key) as I18nContextValue['t'];

test('a queued submission reads as waiting for a judge, not as being judged', () => {
  assert.equal(progressLabel('Queued', t), 'result.waitingForJudge');
  assert.equal(progressLabel('Pending', t), 'result.judging');
  assert.equal(progressLabel('Compiling', t), 'result.compiling');
  assert.equal(progressLabel(undefined, t), 'result.judging');
});

test('a superseded judgement reads as superseded, and no submission filters by it', () => {
  assert.deepEqual(
    getVerdictBadge({ status: 'Superseded', verdict: null }, t),
    {
      label: 'result.superseded',
      variant: 'outline',
    },
  );
  assert.equal(SUBMISSION_STATUS_FILTER_OPTIONS.includes('Superseded'), false);
});

test('a parked submission names what parks it, as the server records it', () => {
  for (const provides of [
    'contest_type',
    'problem_type',
    'checker_format',
  ] as const) {
    assert.deepEqual(
      getVerdictBadge(
        { status: 'Queued', parked_by: { plugin_id: 'p', provides } },
        t,
      ),
      { label: `submissions.parkedBy.${provides}`, variant: 'outline' },
    );
  }
});
