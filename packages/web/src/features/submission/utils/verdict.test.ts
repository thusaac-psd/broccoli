import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { I18nContextValue } from '@broccoli/web-sdk/i18n';

import { progressLabel } from './verdict.ts';

const t = ((key: string) => key) as I18nContextValue['t'];

test('a queued submission reads as waiting for a judge, not as being judged', () => {
  assert.equal(progressLabel('Queued', t), 'result.waitingForJudge');
  assert.equal(progressLabel('Pending', t), 'result.judging');
  assert.equal(progressLabel('Compiling', t), 'result.compiling');
  assert.equal(progressLabel(undefined, t), 'result.judging');
});
