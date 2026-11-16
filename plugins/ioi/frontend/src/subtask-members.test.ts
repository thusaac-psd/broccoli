import assert from 'node:assert/strict';
import { test } from 'node:test';

import { memberCell } from './subtask-members.ts';

test('a member the label map leaves out names no test case and is not pending', () => {
  const cell = memberCell('3', { '1': 11 }, new Map(), -1);
  assert.equal(cell.isMissing, true);
  assert.equal(cell.verdict, null);
  assert.equal(cell.score, null);
});

test('a resolved member waits as pending until its result arrives', () => {
  const pending = memberCell('a', { a: 11 }, new Map(), -1);
  assert.equal(pending.verdict, 'Pending');
  assert.equal(pending.test_case_id, 11);
  assert.equal(pending.isMissing, undefined);

  const result = { id: 5, test_case_id: 11, verdict: 'Accepted', score: 10 };
  assert.equal(memberCell('a', { a: 11 }, new Map([[11, result]]), -1), result);
});
