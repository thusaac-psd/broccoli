import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { I18nContextValue } from '@/i18n';
import type { Provided } from '@/submission/types';

import { SUBMISSION_STATUS_FILTER_OPTIONS } from './utils.ts';
import { getVerdictBadge, progressLabel } from './verdict.ts';

const t = ((key: string, params?: Record<string, string | number>) =>
  params
    ? `${key}(${Object.values(params).join(',')})`
    : key) as I18nContextValue['t'];
/** A catalog that names only the plugin `named`. */
const lookup = (key: string) =>
  key === 'plugin.named.name' ? 'Named Plugin' : undefined;
const i18n = { t, lookup };

test('a queued submission reads as waiting for a judge, not as being judged', () => {
  assert.equal(progressLabel('Queued', t), 'result.waitingForJudge');
  assert.equal(progressLabel('Pending', t), 'result.judging');
  assert.equal(progressLabel('Compiling', t), 'result.compiling');
  assert.equal(progressLabel(undefined, t), 'result.judging');
});

test('a superseded judgement reads as superseded, and no submission filters by it', () => {
  assert.deepEqual(
    getVerdictBadge({ status: 'Superseded', verdict: null }, i18n),
    {
      label: 'result.superseded',
      variant: 'outline',
    },
  );
  assert.equal(SUBMISSION_STATUS_FILTER_OPTIONS.includes('Superseded'), false);
});

test('a parked submission names what parks it and the plugin, as the server records it', () => {
  // Every role `Provided` names, so a new one does not compile untested.
  const roles: Record<Provided, true> = {
    contest_type: true,
    problem_type: true,
    checker_format: true,
    language: true,
    checker_language: true,
  };
  for (const provides of Object.keys(roles) as Provided[]) {
    assert.deepEqual(
      getVerdictBadge(
        {
          status: 'Queued',
          parked_by: { plugin_id: 'named', provides, reason: 'disabled' },
        },
        i18n,
      ),
      {
        label: `submissions.parkedBy.${provides}(Named Plugin)`,
        variant: 'outline',
      },
    );
  }
  // A plugin no catalog names is named by its id.
  assert.equal(
    getVerdictBadge(
      {
        status: 'Queued',
        parked_by: {
          plugin_id: 'unnamed',
          provides: 'contest_type',
          reason: 'disabled',
        },
      },
      i18n,
    ).label,
    'submissions.parkedBy.contest_type(unnamed)',
  );
  // A provider loaded on no server says so, whatever its role.
  assert.equal(
    getVerdictBadge(
      {
        status: 'Queued',
        parked_by: {
          plugin_id: 'named',
          provides: 'checker_format',
          reason: 'not_loaded',
        },
      },
      i18n,
    ).label,
    'submissions.parkedBy.notLoaded(Named Plugin)',
  );
});
