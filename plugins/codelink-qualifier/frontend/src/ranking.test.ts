import assert from 'node:assert/strict';
import { test } from 'node:test';

import { rankStandings } from './ranking.ts';
import type { Standing } from './types';

function contestant(user_id: number, accepted: number, score = 0): Standing {
  return {
    user_id,
    username: `oj_${user_id}`,
    display_name: '同名选手',
    credited: 0,
    score,
    accepted,
    qualification_verdict: null,
    qualified_at_seconds: null,
    problems: {},
  };
}

test('solved ranking counts ACs with no slot and after qualification', () => {
  const allStatuses: Standing = {
    ...contestant(2, 3, 1),
    credited: 1,
    problems: {
      '1': { status: 'credited', slot: 1, submission_id: 1, time_seconds: 10 },
      '2': {
        status: 'slots_full',
        slot: null,
        submission_id: 2,
        time_seconds: 20,
      },
      '3': {
        status: 'after_qualification',
        slot: null,
        submission_id: 3,
        time_seconds: 30,
      },
    },
  };
  const rows = [contestant(1, 2, 4), allStatuses, contestant(3, 1)];
  assert.deepEqual(
    rankStandings(rows, 'accepted').map(({ row }) => row.user_id),
    [2, 1, 3],
  );
});

test('equal solve counts share competition ranks, including zero solves', () => {
  const rows = [
    contestant(5, 0),
    contestant(4, 1),
    contestant(2, 3),
    contestant(1, 3),
    contestant(3, 0),
  ];
  assert.deepEqual(
    rankStandings(rows, 'accepted').map(({ row, rank }) => [row.user_id, rank]),
    [
      [1, 1],
      [2, 1],
      [4, 3],
      [3, 4],
      [5, 4],
    ],
  );
});

test('switching views preserves the official order and cached standings', () => {
  const rows = Object.freeze([
    Object.freeze(contestant(3, 1, 4)),
    Object.freeze(contestant(1, 3, 3)),
    Object.freeze(contestant(2, 2, 1)),
  ]);
  rankStandings(rows, 'accepted');
  assert.deepEqual(
    rankStandings(rows, 'points').map(({ row }) => row.user_id),
    [3, 1, 2],
  );
  assert.deepEqual(
    rows.map((row) => row.user_id),
    [3, 1, 2],
  );
});

test('a refresh or rejudge updates solved ranks without stale tie positions', () => {
  const rows = [contestant(1, 3), contestant(2, 2)];
  assert.deepEqual(
    rankStandings(rows, 'accepted').map(({ rank }) => rank),
    [1, 2],
  );
  const rejudged = [contestant(1, 1), contestant(2, 2)];
  assert.deepEqual(
    rankStandings(rejudged, 'accepted').map(({ row, rank }) => [
      row.user_id,
      rank,
    ]),
    [
      [2, 1],
      [1, 2],
    ],
  );
  assert.deepEqual(rankStandings([], 'accepted'), []);
});
