import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  CONTEST_PROBLEMS_REFRESH_MS,
  problemListRefetchInterval,
} from './problem-refresh.ts';

test('the problem list polls only while the server says the contest runs', () => {
  assert.equal(problemListRefetchInterval({ phase: 'upcoming' }), false);
  assert.equal(
    problemListRefetchInterval({ phase: 'running' }),
    CONTEST_PROBLEMS_REFRESH_MS,
  );
  assert.equal(problemListRefetchInterval({ phase: 'ended' }), false);
});

test('no polling before the contest is known', () => {
  assert.equal(problemListRefetchInterval(undefined), false);
});
