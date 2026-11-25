import assert from 'node:assert/strict';
import { test } from 'node:test';

import { submitClosure } from './submit-closure.ts';

const CONTEST_OUT = { state: 'disabled', plugin_id: 'icpc' } as const;
const EVALUATOR = { plugin_id: 'batch', provides: 'problem_type' } as const;

test("a contest's plugin outage is the one notice under the contest", () => {
  assert.deepEqual(
    submitClosure('contest', CONTEST_OUT, {
      closed_by: { plugin_id: 'icpc', provides: 'contest_type' },
    }),
    { kind: 'contest', outage: CONTEST_OUT },
  );
});

test('a disabled evaluator closes submitting in a contest and in practice', () => {
  for (const scope of ['contest', 'practice'] as const) {
    assert.deepEqual(submitClosure(scope, null, { closed_by: EVALUATOR }), {
      kind: 'closed',
      closedBy: EVALUATOR,
    });
  }
});

test("a practice problem's contest type out of reach here is unavailable", () => {
  const problem = { plugin_outage: { state: 'unavailable' } } as const;
  assert.deepEqual(submitClosure('practice', null, problem), {
    kind: 'unavailable',
  });
  // Under a contest the problem's practice type does not decide.
  assert.equal(submitClosure('contest', null, problem), null);
});

test('nothing closes submitting while every plugin it needs serves', () => {
  assert.equal(submitClosure('practice', null, {}), null);
  assert.equal(submitClosure('contest', null, undefined), null);
});

test('a problem with no language open to submit in is closed, never offered as plain text', () => {
  assert.deepEqual(submitClosure('practice', null, {}, 0), {
    kind: 'noLanguage',
  });
  // What the server closes says more, so it comes first.
  const language = { plugin_id: 'langs', provides: 'language' } as const;
  assert.deepEqual(
    submitClosure('practice', null, { closed_by: language }, 0),
    {
      kind: 'closed',
      closedBy: language,
    },
  );
  // Languages still loading close nothing yet.
  assert.equal(submitClosure('practice', null, {}, undefined), null);
  assert.equal(submitClosure('practice', null, {}, 2), null);
});
