import assert from 'node:assert/strict';
import { test } from 'node:test';

import { tokenTarget } from './token-target.ts';

const usable = [
  { submission_id: 8, problem_id: 1 },
  { submission_id: 7, problem_id: 2 },
  { submission_id: 5, problem_id: 1 },
];

test('a token goes to the submission shown when it can take one', () => {
  assert.equal(tokenTarget(usable, 1, 5), 5);
});

test('a token goes to the newest usable submission of the problem otherwise', () => {
  // Submission 9 is still judging, or already tokened: the plugin does not
  // list it, so the newest one it lists for this problem is offered.
  assert.equal(tokenTarget(usable, 1, 9), 8);
  assert.equal(tokenTarget(usable, 1, undefined), 8);
  assert.equal(tokenTarget(usable, 2, 9), 7);
  assert.equal(tokenTarget(usable, 3, undefined), undefined);
});
