// A per-test-case `verdict: null` is ambiguous between "not yet judged" and
// "judged, but redacted by a feedback-level field mask". `getVerdictKey`
// tells them apart by the enclosing submission or judgement `status`, which
// no mask touches. Both directions must hold: always rendering `null` as
// "Skipped" would mislabel test cases that are still running.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { SubmissionStatus } from '@broccoli/web-sdk/submission';

import { getVerdictKey } from './verdict-key.ts';

const TERMINAL_STATUSES: SubmissionStatus[] = [
  'Judged',
  'CompilationError',
  'SystemError',
];

const NON_TERMINAL_STATUSES: SubmissionStatus[] = [
  'Queued',
  'Pending',
  'Compiling',
];

for (const status of TERMINAL_STATUSES) {
  test(`a null verdict under terminal status "${status}" is a redaction, not a pending case`, () => {
    assert.equal(getVerdictKey(null, status), 'skipped');
  });

  test(`an undefined verdict under terminal status "${status}" is a redaction, not a pending case`, () => {
    assert.equal(getVerdictKey(undefined, status), 'skipped');
  });
}

// `Running` is not terminal (pollers keep polling while `Running`), but a
// null verdict on an existing test-case row is still a redaction, not "not
// run yet": a case can finish and be masked while the judgement as a whole
// is still `Running`.
for (const status of ['Running'] as const satisfies SubmissionStatus[]) {
  test(`a null verdict under status "${status}" is a redaction, not a pending case`, () => {
    assert.equal(getVerdictKey(null, status), 'skipped');
  });

  test(`an undefined verdict under status "${status}" is a redaction, not a pending case`, () => {
    assert.equal(getVerdictKey(undefined, status), 'skipped');
  });
}

for (const status of NON_TERMINAL_STATUSES) {
  test(`a null verdict under non-terminal status "${status}" is genuinely pending`, () => {
    assert.equal(getVerdictKey(null, status), 'pending');
  });

  test(`an undefined verdict under non-terminal status "${status}" is genuinely pending`, () => {
    assert.equal(getVerdictKey(undefined, status), 'pending');
  });
}

test('a genuine (unmasked) "Skipped" verdict is always "skipped", regardless of status', () => {
  assert.equal(getVerdictKey('Skipped', 'Judged'), 'skipped');
  assert.equal(getVerdictKey('Skipped', 'Running'), 'skipped');
  assert.equal(getVerdictKey('Skipped', 'Pending'), 'skipped');
});

test('non-null verdicts map to their own key regardless of status', () => {
  assert.equal(getVerdictKey('Accepted', 'Running'), 'accepted');
  assert.equal(getVerdictKey('WrongAnswer', 'Judged'), 'wrong_answer');
  assert.equal(getVerdictKey('TimeLimitExceeded', 'Judged'), 'time_limit');
  assert.equal(getVerdictKey('MemoryLimitExceeded', 'Judged'), 'memory_limit');
  assert.equal(getVerdictKey('RuntimeError', 'Judged'), 'runtime_error');
  assert.equal(getVerdictKey('SystemError', 'Judged'), 'system_error');
  assert.equal(getVerdictKey('Cancelled', 'Judged'), 'cancelled');
});

test('an unrecognized verdict string falls back to "custom"', () => {
  assert.equal(
    getVerdictKey('SomeFuturePluginVerdict' as never, 'Judged'),
    'custom',
  );
});
