import assert from 'node:assert/strict';
import { test } from 'node:test';

import { configBreakIssues } from './config-break.ts';

const issue = {
  plugin_id: 'ioi',
  scope: 'contest_problem',
  ref_id: '1:2',
  namespace: 'task',
  errors: [
    {
      path: 'subtasks[1].test_cases[0]',
      message: 'no such test case',
      kind: 'plugin',
    },
  ],
};

test('a CONFIG_WOULD_BREAK answer lists the configs it would break', () => {
  assert.deepEqual(
    configBreakIssues({
      code: 'CONFIG_WOULD_BREAK',
      details: { issues: [issue] },
    }),
    [issue],
  );
});

test('any other outcome breaks nothing', () => {
  assert.equal(configBreakIssues(undefined), null);
  assert.equal(
    configBreakIssues({ code: 'CONFLICT', message: 'duplicate' }),
    null,
  );
});
