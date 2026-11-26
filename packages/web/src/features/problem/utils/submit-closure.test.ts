import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { PluginOutage } from '@broccoli/web-sdk/submission';

import {
  languagesClosed,
  problemClosed,
  problemLanguages,
  submitClosure,
} from './submit-closure.ts';

const CONTEST_OUT = { state: 'disabled', plugin_id: 'icpc' } as const;
const EVALUATOR = {
  plugin_id: 'batch',
  provides: 'problem_type',
  reason: 'disabled',
} as const;

test("a contest's plugin outage is the one notice under the contest", () => {
  assert.deepEqual(
    submitClosure('contest', CONTEST_OUT, {
      closed_by: {
        plugin_id: 'icpc',
        provides: 'contest_type',
        reason: 'disabled',
      },
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

// Every state a practice problem's contest type can be out in: a state the
// server adds fails to compile here until it is listed.
const PRACTICE_OUTAGES = {
  disabled: { state: 'disabled', plugin_id: 'icpc' },
  not_loaded: { state: 'not_loaded', plugin_id: 'icpc' },
  not_loaded_here: { state: 'not_loaded_here', plugin_id: 'icpc' },
  unavailable: { state: 'unavailable' },
} as const satisfies {
  [S in PluginOutage['state']]: PluginOutage & { state: S };
};

test("a practice problem's contest type out in any state closes it, and is asked for again", () => {
  for (const outage of Object.values(PRACTICE_OUTAGES)) {
    const problem = { plugin_outage: outage };
    assert.deepEqual(
      submitClosure('practice', null, problem),
      { kind: 'practice', outage },
      outage.state,
    );
    assert.equal(problemClosed('practice', problem), true, outage.state);
    // Under a contest the problem's practice type does not decide.
    assert.equal(submitClosure('contest', null, problem), null, outage.state);
    assert.equal(problemClosed('contest', problem), false, outage.state);
  }
});

test('nothing closes submitting while every plugin it needs serves', () => {
  assert.equal(submitClosure('practice', null, {}), null);
  assert.equal(submitClosure('contest', null, undefined), null);
});

const LANGS = {
  plugin_id: 'langs',
  provides: 'language',
  reason: 'disabled',
} as const;

function language(id: string) {
  return {
    id,
    name: id,
    template: '',
    defaultFilename: `main.${id}`,
    extensions: [id],
  };
}

const NO_OPEN = { open: [], closed: [] };

test('a problem with no language open to submit in is closed, never offered as plain text', () => {
  assert.deepEqual(submitClosure('practice', null, {}, NO_OPEN), {
    kind: 'noLanguage',
  });
  // What the server closes says more, so it comes first.
  assert.deepEqual(
    submitClosure('practice', null, { closed_by: LANGS }, NO_OPEN),
    { kind: 'closed', closedBy: LANGS },
  );
  // Languages still loading close nothing yet.
  assert.equal(submitClosure('practice', null, {}, undefined), null);
  assert.equal(
    submitClosure(
      'practice',
      null,
      {},
      {
        open: [language('cpp')],
        closed: [],
      },
    ),
    null,
  );
});

test('with no language open, the plugin that closes one names the closure', () => {
  // The registries lost every language the problem takes before the
  // problem says it is closed: the plugin it does name is the reason.
  assert.deepEqual(
    submitClosure(
      'practice',
      null,
      {},
      {
        open: [],
        closed: [{ language: 'java', closed_by: LANGS }],
      },
    ),
    { kind: 'closed', closedBy: LANGS },
  );
});

test("a problem's languages are those its format names, of those the server takes, less the closed ones", () => {
  const supported = [language('c'), language('cpp'), language('java')];
  const closed = [{ language: 'java', closed_by: LANGS }];
  assert.deepEqual(
    problemLanguages(supported, { closed_languages: closed }).open.map(
      (l) => l.id,
    ),
    ['c', 'cpp'],
  );
  // A format names the languages the server accepts, and no other: one it
  // names that the server cannot take now leaves none open, never another.
  const format = { java: ['Main.java'] };
  assert.deepEqual(
    problemLanguages(supported, {
      submission_format: format,
      closed_languages: closed,
    }),
    { open: [], closed },
  );
  assert.deepEqual(
    problemLanguages([language('c')], { submission_format: format }).open,
    [],
  );
  // An empty format names none, so every language is open.
  assert.equal(
    problemLanguages(supported, { submission_format: {} }).open.length,
    3,
  );
});

test('what is closed is asked for again, and nothing else is', () => {
  assert.equal(languagesClosed({ open: [language('c')], closed: [] }), false);
  assert.equal(languagesClosed(NO_OPEN), true);
  assert.equal(
    languagesClosed({
      open: [language('c')],
      closed: [{ language: 'java', closed_by: LANGS }],
    }),
    true,
  );
  assert.equal(problemClosed('practice', {}), false);
  assert.equal(problemClosed('practice', undefined), false);
  assert.equal(problemClosed('contest', { closed_by: EVALUATOR }), true);
  assert.equal(
    problemClosed('contest', {
      closed_languages: [{ language: 'java', closed_by: LANGS }],
    }),
    true,
  );
  const unreachable = { plugin_outage: { state: 'unavailable' } } as const;
  assert.equal(problemClosed('practice', unreachable), true);
  // Under a contest, the contest's own outage is polled with the contest.
  assert.equal(problemClosed('contest', unreachable), false);
});
