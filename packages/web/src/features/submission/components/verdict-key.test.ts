// Pure-logic test for `getVerdictKey` (see `verdict-key.ts`). The repo has
// no frontend test runner configured (`pnpm test` at the root is a no-op
// placeholder) and none is introduced by this change; this file runs
// standalone via Node's built-in test runner + native TypeScript support
// (`node --test verdict-key.test.ts`, Node >= 22), with no new dependency
// and no bundler/DOM needed, since the function under test is pure and has
// no React/JSX involved.
//
// This test exists to lock in the fix for the CRITICAL code-review finding
// on task 16: a per-test-case `verdict: null` is ambiguous between "not yet
// judged" and "judged, but redacted by an IOI feedback-level field mask"
// (`subtask_scores`/`total_only`). `getVerdictKey` disambiguates using the
// enclosing submission/judgement `status`, which no IOI mask ever touches.
// Both directions must hold: a naive fix that always renders `null` as
// "Skipped" would incorrectly label genuinely in-flight test cases as
// skipped while a submission is still running.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { SubmissionStatus } from '@broccoli/web-sdk/submission';

import { getVerdictKey } from './verdict-key.ts';

const TERMINAL_STATUSES: SubmissionStatus[] = [
  'Judged',
  'CompilationError',
  'SystemError',
];

// M16: `Queued` is a valid `SubmissionStatus` (see the `SubmissionStatus`
// schema literal in `@broccoli/web-sdk/api/schema`) but was missing here -
// this suite exercised every non-terminal status except the very first one
// a submission can be in.
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

// `Running` is deliberately NOT in `TERMINAL_STATUSES` (see
// `@broccoli/web-sdk/submission`'s `isTerminalStatus` -- pollers must keep
// polling while `Running`) but a null verdict on an existing test-case row
// must still be treated as a redaction here, not "not run yet": a case can
// finish and be masked while the judgement as a whole is still `Running`.
// Task 17 / Step 2z: this pins the CRITICAL finding from the Task 16
// re-review -- the previous version of this test asserted 'pending' for
// this exact case, which was pinning the bug rather than the fix.
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
