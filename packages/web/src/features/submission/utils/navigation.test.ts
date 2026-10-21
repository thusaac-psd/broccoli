import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { User } from '@broccoli/web-sdk/auth';
import {
  PROBLEM_CREATE,
  SUBMISSION_VIEW_ALL,
} from '@broccoli/web-sdk/permissions';

import {
  createSubmissionNavigationState,
  getSubmissionScrollKey,
  getSubmissionScrollRestoration,
  readSubmissionReturnContext,
  recordNavigation,
  resolveSubmissionReturn,
  submissionReturnState,
} from './navigation.ts';

const setter: User = {
  id: 7,
  username: 'setter',
  display_name: 'Setter',
  roles: ['setter'],
  permissions: [SUBMISSION_VIEW_ALL, PROBLEM_CREATE],
};
const contestant: User = { ...setter, permissions: [], roles: ['user'] };

function location(href: string, state: unknown = null) {
  const url = new URL(href, 'https://broccoli.invalid');
  return {
    pathname: url.pathname,
    search: url.search,
    hash: url.hash,
    key: 'source',
    state,
  };
}

const unavailable = {
  canViewAll: false,
  canViewContest: false,
  canViewProblem: false,
};

test('all entry points preserve their complete source and select a matching label', () => {
  const cases = [
    [
      '/admin/submissions?page=4&q=Bay&language=cpp&status=Judged#row-98',
      'allSubmissions',
      'backToAllSubmissions',
    ],
    [
      '/contests/3/submissions?page=2&problem=9&status=Running',
      'contestSubmissions',
      'backToContestSubmissions',
    ],
    ['/problems/9?tab=coding#editor', 'coding', 'backToCoding'],
    ['/contests/3/problems/9?tab=coding', 'coding', 'backToCoding'],
    ['/admin', 'overview', 'backToOverview'],
  ];
  for (const [href, kind, label] of cases) {
    const state = createSubmissionNavigationState(location(href), setter);
    const source = readSubmissionReturnContext(state, setter);
    assert.equal(source?.kind, kind);
    assert.equal(source?.href, href);
    assert.equal(source?.locationKey, 'source');
    // The submission belongs to a contest, but its origin still wins. The
    // same origin also works when loading a submission fails (no IDs known).
    for (const contestId of [3, undefined]) {
      const target = resolveSubmissionReturn({
        ...unavailable,
        source,
        contestId,
        canViewContest: true,
      });
      assert.equal(target.to, href);
      assert.equal(target.labelKey, 'submissionDetail.' + label);
    }
  }
});

test('source is bound to the signed-in user and current permissions, not role names', () => {
  const state = createSubmissionNavigationState(
    location('/admin/submissions?page=3'),
    setter,
  );
  assert.ok(readSubmissionReturnContext(state, setter));
  assert.equal(readSubmissionReturnContext(state, null), undefined);
  assert.equal(
    readSubmissionReturnContext(state, { ...setter, id: 8 }),
    undefined,
  );
  assert.equal(readSubmissionReturnContext(state, contestant), undefined);
  assert.equal(
    createSubmissionNavigationState(location('/admin'), contestant),
    undefined,
  );
  assert.ok(
    createSubmissionNavigationState(
      location('/problems/9?tab=coding'),
      contestant,
    ),
  );
});

test('unrecognized, external, malformed and mismatched source state is discarded', () => {
  const state = createSubmissionNavigationState(
    location('/admin/submissions'),
    setter,
  )!;
  for (const href of [
    'https://elsewhere.test/admin/submissions',
    '//elsewhere.test/admin/submissions',
    '/\\elsewhere.test/admin/submissions',
    '/admin/../admin/submissions',
    '/admin/submissions\n',
    '/submissions/98',
    '/problems/9?tab=edit',
    '/contests/0/submissions',
    '/contests/-3/submissions',
  ]) {
    assert.equal(
      readSubmissionReturnContext(
        { submissionReturn: { ...state.submissionReturn, href } },
        setter,
      ),
      undefined,
      href,
    );
  }
  for (const patch of [
    { kind: 'overview' },
    { locationKey: '' },
    { scrollKey: null },
    { href: 5 },
    { scrollTop: -1 },
    { scrollTop: Infinity },
  ]) {
    assert.equal(
      readSubmissionReturnContext(
        { submissionReturn: { ...state.submissionReturn, ...patch } },
        setter,
      ),
      undefined,
    );
  }
  for (const value of [
    null,
    undefined,
    false,
    1,
    'bad',
    {},
    { submissionReturn: {} },
  ]) {
    assert.equal(readSubmissionReturnContext(value, setter), undefined);
  }
});

test('direct links fall back from an accessible contest to all submissions, problem, then home', () => {
  const input = {
    contestId: 3,
    problemId: 9,
    canViewAll: true,
    canViewContest: true,
    canViewProblem: true,
  };
  assert.deepEqual(resolveSubmissionReturn(input), {
    to: '/contests/3/submissions',
    labelKey: 'submissionDetail.viewContestSubmissions',
  });
  assert.deepEqual(
    resolveSubmissionReturn({ ...input, canViewContest: false }),
    {
      to: '/admin/submissions',
      labelKey: 'submissionDetail.viewAllSubmissions',
    },
  );
  assert.deepEqual(
    resolveSubmissionReturn({
      ...input,
      canViewContest: false,
      canViewAll: false,
    }),
    {
      to: '/problems/9',
      labelKey: 'submissionDetail.openProblem',
    },
  );
  assert.deepEqual(resolveSubmissionReturn({ ...input, ...unavailable }), {
    to: '/',
    labelKey: 'error.backToHome',
  });
  assert.equal(
    resolveSubmissionReturn(unavailable).labelKey,
    'error.backToHome',
  );
});

test('explicit returns restore only the recorded source scroll entry, including repeated visits', () => {
  const href = '/admin/submissions?page=3#row-98';
  const source = readSubmissionReturnContext(
    createSubmissionNavigationState(location(href), setter, 946),
    setter,
  )!;
  const target = resolveSubmissionReturn({ ...unavailable, source });
  const returned = {
    ...location(href, submissionReturnState(target)),
    key: 'replaced-detail',
  };
  assert.equal(getSubmissionScrollKey(returned), 'source');
  assert.equal(getSubmissionScrollRestoration(returned)?.top, 946);
  assert.equal(
    getSubmissionScrollKey({ ...returned, search: '?page=4' }),
    'replaced-detail',
  );
  const nextSource = readSubmissionReturnContext(
    createSubmissionNavigationState(returned, setter),
    setter,
  )!;
  assert.equal(nextSource.locationKey, 'replaced-detail');
  assert.equal(nextSource.scrollKey, 'source');
  assert.equal(
    submissionReturnState(resolveSubmissionReturn(unavailable)),
    undefined,
  );
});

test('history tracks the actual previous entry across back, forward and replacement', () => {
  let history = { keys: ['landing'], index: 0 };
  history = recordNavigation(history, 'source', 'PUSH');
  history = recordNavigation(history, 'detail', 'PUSH');
  assert.equal(history.keys[history.index - 1], 'source');
  history = recordNavigation(history, 'source', 'POP');
  history = recordNavigation(history, 'detail', 'POP');
  assert.equal(history.keys[history.index - 1], 'source');
  history = recordNavigation(history, 'other', 'REPLACE');
  assert.deepEqual(history.keys, ['landing', 'source', 'other']);
  history = recordNavigation(history, 'source', 'POP');
  history = recordNavigation(history, 'new-detail', 'PUSH');
  assert.deepEqual(history.keys, ['landing', 'source', 'new-detail']);
});

test('a reload, new tab or unknown history entry never assumes a previous source', () => {
  const initial = { keys: ['detail'], index: 0 };
  assert.equal(recordNavigation(initial, 'detail', 'POP'), initial);
  assert.equal(initial.keys[initial.index - 1], undefined);
  const unknown = recordNavigation(
    { keys: ['source', 'detail'], index: 1 },
    'outside-app',
    'POP',
  );
  assert.deepEqual(unknown, { keys: ['outside-app'], index: 0 });
});
