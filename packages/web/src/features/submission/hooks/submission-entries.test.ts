import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { Submission } from '@broccoli/web-sdk/submission';

import {
  entriesReducer,
  type EntryAction,
  type EntryResult,
  pollEntry,
  type PollResponse,
  pollTarget,
  type SubmissionEntry,
} from './submission-entries.ts';

function entry(id: number, status: SubmissionEntry['status']): SubmissionEntry {
  return { id, submission: null, codeRun: null, status, error: null };
}

function result(status: string, terminal: boolean): EntryResult {
  return { submission: { id: 41, status } as unknown as Submission, terminal };
}

const error = { code: 'NOT_FOUND', message: 'gone' };

test('an accepted submission is polled until a final result settles it', () => {
  let entries = entriesReducer([], {
    type: 'added',
    entries: [entry(1, 'submitting')],
  });
  entries = entriesReducer(entries, {
    type: 'accepted',
    id: 1,
    result: result('Pending', false),
  });
  assert.equal(entries[0].status, 'polling');
  entries = entriesReducer(entries, {
    type: 'polled',
    id: 1,
    result: result('Judged', true),
  });
  assert.equal(entries[0].status, 'done');
  assert.equal(entries[0].submission?.status, 'Judged');
});

test('a submission accepted by id alone is polled by that id', () => {
  let entries = entriesReducer([entry(1, 'submitting')], {
    type: 'accepted',
    id: 1,
    submissionId: 41,
  });
  assert.equal(entries[0].status, 'polling');
  assert.equal(entries[0].submission, null);
  assert.deepEqual(pollTarget(entries[0]), { kind: 'submission', id: 41 });
  entries = entriesReducer(entries, {
    type: 'polled',
    id: 1,
    result: result('Judged', true),
  });
  assert.equal(entries[0].status, 'done');
  assert.equal(entries[0].submission?.status, 'Judged');
});

test('an accept by id moves only a submitting entry', () => {
  for (const status of ['polling', 'done', 'error'] as const) {
    const entries = [entry(1, status)];
    assert.equal(
      entriesReducer(entries, { type: 'accepted', id: 1, submissionId: 41 }),
      entries,
    );
  }
});

test('a late non-final poll cannot move a done entry back to polling', () => {
  const done = [
    { ...entry(1, 'done'), submission: result('Judged', true).submission! },
  ];
  const next = entriesReducer(done, {
    type: 'polled',
    id: 1,
    result: result('Running', false),
  });
  assert.equal(next, done);
});

test('an entry that failed stays failed', () => {
  const failed = [entry(1, 'error')];
  for (const action of [
    { type: 'accepted', id: 1, result: result('Pending', false) },
    { type: 'polled', id: 1, result: result('Judged', true) },
    { type: 'failed', id: 1, error },
  ] satisfies EntryAction[]) {
    assert.equal(entriesReducer(failed, action), failed);
  }
});

test('a refused submit is a rejection, never a submission being judged', () => {
  const rejected = entriesReducer([entry(1, 'submitting')], {
    type: 'rejected',
    id: 1,
    error,
  });
  assert.equal(rejected[0].status, 'rejected');
  assert.equal(rejected[0].error, error);
  assert.equal(pollTarget(rejected[0]), null);
  // Nothing the server says later turns it into a submission.
  for (const action of [
    { type: 'accepted', id: 1, result: result('Pending', false) },
    { type: 'polled', id: 1, result: result('Judged', true) },
    { type: 'failed', id: 1, error },
  ] satisfies EntryAction[]) {
    assert.equal(entriesReducer(rejected, action), rejected);
  }
});

test('only a submit still in flight can be refused', () => {
  const polling = [entry(1, 'polling')];
  assert.equal(
    entriesReducer(polling, { type: 'rejected', id: 1, error }),
    polling,
  );
});

test('the next attempt replaces an earlier rejection', () => {
  const rejected = entriesReducer([entry(1, 'submitting')], {
    type: 'rejected',
    id: 1,
    error,
  });
  const next = entriesReducer(rejected, {
    type: 'added',
    entries: [entry(2, 'submitting')],
  });
  assert.deepEqual(
    next.map((e) => e.id),
    [2],
  );
});

test('a submit that settles after the entries were reset changes nothing', () => {
  const entries = entriesReducer([entry(1, 'submitting')], { type: 'reset' });
  assert.deepEqual(
    entriesReducer(entries, {
      type: 'accepted',
      id: 1,
      result: result('Pending', false),
    }),
    [],
  );
});

test('a run result lands in the code run, not the submission', () => {
  const run = [{ ...entry(1, 'submitting'), isRun: true }];
  const codeRun = { id: 9, status: 'Pending' } as never;
  const next = entriesReducer(run, {
    type: 'accepted',
    id: 1,
    result: { codeRun, terminal: false },
  });
  assert.equal(next[0].codeRun, codeRun);
  assert.equal(next[0].submission, null);
});

function poller(responses: PollResponse[], signedOut = false) {
  const actions: EntryAction[] = [];
  const sleeps: number[] = [];
  const controller = new AbortController();
  let fetches = 0;
  const run = pollEntry(1, controller.signal, {
    fetch: async () => {
      fetches += 1;
      const next = responses.shift();
      if (!next) {
        controller.abort();
        return { ok: false, status: null, error: 'aborted' };
      }
      return next;
    },
    sleep: async (ms) => {
      sleeps.push(ms);
    },
    dispatch: (action) => actions.push(action),
    isSignedOut: () => signedOut,
  });
  return { run, actions, sleeps, fetches: () => fetches };
}

test('polling stops at the first final result', async () => {
  const p = poller([
    { ok: true, result: result('Running', false) },
    { ok: true, result: result('Judged', true) },
    { ok: true, result: result('Judged', true) },
  ]);
  await p.run;
  assert.equal(p.fetches(), 2);
  assert.deepEqual(
    p.actions.map((a) => a.type),
    ['polled', 'polled'],
  );
});

for (const [status, signedOut] of [
  [403, false],
  [404, false],
  [401, true],
] as const) {
  test(`a ${status}${signedOut ? ' when signed out' : ''} fails the entry and stops polling`, async () => {
    const p = poller(
      [
        { ok: false, status, error },
        { ok: true, result: result('Judged', true) },
      ],
      signedOut,
    );
    await p.run;
    assert.equal(p.fetches(), 1);
    assert.deepEqual(p.actions, [{ type: 'failed', id: 1, error }]);
  });
}

test('a 401 while the session is renewing its token keeps polling', async () => {
  const p = poller([
    { ok: false, status: 401, error: {} },
    { ok: true, result: result('Judged', true) },
  ]);
  await p.run;
  assert.deepEqual(
    p.actions.map((a) => a.type),
    ['polled'],
  );
});

test('transient failures back off and polling resumes its pace after', async () => {
  const p = poller([
    { ok: false, status: 502, error: {} },
    { ok: false, status: null, error: new TypeError('offline') },
    { ok: true, result: result('Running', false) },
    { ok: true, result: result('Judged', true) },
  ]);
  await p.run;
  assert.deepEqual(p.sleeps, [1_000, 2_000, 4_000, 1_000]);
  assert.equal(p.actions.at(-1)?.type, 'polled');
});

test('an aborted poller sends nothing more', async () => {
  const controller = new AbortController();
  controller.abort();
  let fetches = 0;
  await pollEntry(1, controller.signal, {
    fetch: async () => {
      fetches += 1;
      return { ok: true, result: result('Judged', true) };
    },
    sleep: async () => {},
    dispatch: () => assert.fail('dispatched after abort'),
    isSignedOut: () => false,
  });
  assert.equal(fetches, 0);
});
