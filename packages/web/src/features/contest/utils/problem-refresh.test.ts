import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  CONTEST_PROBLEMS_REFRESH_MS,
  problemListRefetchInterval,
} from './problem-refresh.ts';

const contest = {
  start_time: '2026-09-27T09:00:00Z',
  end_time: '2026-09-27T12:00:00Z',
};
const at = (iso: string) => Date.parse(iso);

test('the problem list polls only while the contest runs', () => {
  assert.equal(
    problemListRefetchInterval(contest, at('2026-09-27T08:59:59Z')),
    false,
  );
  assert.equal(
    problemListRefetchInterval(contest, at('2026-09-27T09:00:00Z')),
    CONTEST_PROBLEMS_REFRESH_MS,
  );
  assert.equal(
    problemListRefetchInterval(contest, at('2026-09-27T11:59:59Z')),
    CONTEST_PROBLEMS_REFRESH_MS,
  );
  assert.equal(
    problemListRefetchInterval(contest, at('2026-09-27T12:00:00Z')),
    false,
  );
});

test('no polling before the contest is known', () => {
  assert.equal(problemListRefetchInterval(undefined, Date.now()), false);
});
