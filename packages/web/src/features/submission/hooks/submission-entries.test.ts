import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { Submission } from '@broccoli/web-sdk/submission';

import {
  entriesReducer,
  type EntryAction,
  entryInProgress,
  type EntryResult,
  isOutageRefusal,
  pollEntry,
  type PollResponse,
  pollTarget,
  type SubmissionEntry,
} from './submission-entries.ts';

function entry(id: number, status: SubmissionEntry['status']): SubmissionEntry {
  return { id, submission: null, codeRun: null, status, error: null };
}

function result(
  status: string | null,
  extra: Partial<Submission> = {},
): EntryResult {
  return {
    submission: { id: 41, status, ...extra } as unknown as Submission,
  };
}

const DISABLED = { state: 'disabled', plugin_id: 'icpc' } as const;
const PARKED = {
  plugin_id: 'icpc',
  provides: 'contest_type',
  reason: 'disabled',
} as const;

const error = { code: 'NOT_FOUND', message: 'gone' };

test('an accepted submission is polled until a final result settles it', () => {
  let entries = entriesReducer([], {
    type: 'added',
    entries: [entry(1, 'submitting')],
  });
  entries = entriesReducer(entries, {
    type: 'accepted',
    id: 1,
    result: result('Pending'),
  });
  assert.equal(entries[0].status, 'polling');
  entries = entriesReducer(entries, {
    type: 'polled',
    id: 1,
    result: result('Judged'),
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
  assert.deepEqual(pollTarget(entries[0]), {
    kind: 'submission',
    id: 41,
    pace: 1_000,
  });
  entries = entriesReducer(entries, {
    type: 'polled',
    id: 1,
    result: result('Judged'),
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
    { ...entry(1, 'done'), submission: result('Judged').submission! },
  ];
  const next = entriesReducer(done, {
    type: 'polled',
    id: 1,
    result: result('Running'),
  });
  assert.equal(next, done);
});

test('an entry that failed stays failed', () => {
  const failed = [entry(1, 'error')];
  for (const action of [
    { type: 'accepted', id: 1, result: result('Pending') },
    { type: 'polled', id: 1, result: result('Judged') },
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
    { type: 'accepted', id: 1, result: result('Pending') },
    { type: 'polled', id: 1, result: result('Judged') },
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
      result: result('Pending'),
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
    result: { codeRun },
  });
  assert.equal(next[0].codeRun, codeRun);
  assert.equal(next[0].submission, null);
});

function poller(
  responses: PollResponse[],
  signedOut = false,
  random = () => 1,
) {
  const actions: EntryAction[] = [];
  const sleeps: number[] = [];
  const controller = new AbortController();
  let fetches = 0;
  const run = pollEntry(1, 1_000, controller.signal, {
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
    random,
  });
  return { run, actions, sleeps, fetches: () => fetches };
}

test('polling stops at the first final result', async () => {
  const p = poller([
    { ok: true, result: result('Running') },
    { ok: true, result: result('Judged') },
    { ok: true, result: result('Judged') },
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
        { ok: true, result: result('Judged') },
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
    { ok: true, result: result('Judged') },
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
    { ok: true, result: result('Running') },
    { ok: true, result: result('Judged') },
  ]);
  await p.run;
  assert.deepEqual(p.sleeps, [1_000, 2_000, 4_000, 1_000]);
  assert.equal(p.actions.at(-1)?.type, 'polled');
});

test('an aborted poller sends nothing more', async () => {
  const controller = new AbortController();
  controller.abort();
  let fetches = 0;
  await pollEntry(1, 1_000, controller.signal, {
    fetch: async () => {
      fetches += 1;
      return { ok: true, result: result('Judged') };
    },
    sleep: async () => {},
    dispatch: () => assert.fail('dispatched after abort'),
    isSignedOut: () => false,
  });
  assert.equal(fetches, 0);
});

test('a submission its contest plugin parks keeps polling, at the waiting pace', async () => {
  const p = poller([
    { ok: true, result: result('Queued', { parked_by: PARKED }) },
    {
      ok: true,
      result: result(null, {
        withheld: { reason: 'plugin_outage', outage: DISABLED },
      }),
    },
    { ok: true, result: result('Compiling') },
    { ok: true, result: result('Judged') },
  ]);
  await p.run;
  assert.equal(p.fetches(), 4);
  assert.deepEqual(p.sleeps, [1_000, 30_000, 30_000, 1_000]);
});

test('an entry whose outcome a plugin outage withholds is not done', () => {
  const entries = entriesReducer([entry(1, 'polling')], {
    type: 'polled',
    id: 1,
    result: result(null, {
      withheld: { reason: 'plugin_outage', outage: { state: 'unavailable' } },
    }),
  });
  assert.equal(entries[0].status, 'polling');
  assert.equal(pollTarget(entries[0])?.pace, 30_000);
});

test('an entry whose outcome the contest hides is done, and polling stops', async () => {
  const hidden = result(null, { withheld: { reason: 'hidden' } });
  const entries = entriesReducer([entry(1, 'polling')], {
    type: 'polled',
    id: 1,
    result: hidden,
  });
  assert.equal(entries[0].status, 'done');
  const p = poller([
    { ok: true, result: hidden },
    { ok: true, result: result('Judged') },
  ]);
  await p.run;
  assert.equal(p.fetches(), 1);
});

test('a done entry whose submission became visible is read again, by its submission only', () => {
  const hidden = result(null, { withheld: { reason: 'hidden' } });
  const done = entriesReducer([entry(1, 'polling'), entry(2, 'polling')], {
    type: 'polled',
    id: 1,
    result: hidden,
  });
  const entries = entriesReducer(done, {
    type: 'reread',
    submissionId: 41,
    result: result('Judged'),
  });
  assert.equal(entries[0].status, 'done');
  assert.equal(entries[0].submission?.status, 'Judged');
  // An entry still being polled is left to its poller.
  assert.equal(entries[1].status, 'polling');
  assert.equal(entries[1].submission, null);
  const other = entriesReducer(entries, {
    type: 'reread',
    submissionId: 99,
    result: result('Judged'),
  });
  assert.equal(other, entries, 'another submission changes nothing');
});

test('failing polls back off with jitter', async () => {
  const p = poller(
    [
      { ok: false, status: 502, error: {} },
      { ok: false, status: 502, error: {} },
      { ok: true, result: result('Judged') },
    ],
    false,
    () => 0,
  );
  await p.run;
  // Equal jitter at its low end: half of each doubled wait, never below the pace.
  assert.deepEqual(p.sleeps, [1_000, 1_000, 2_000]);
});

test('an entry is in progress only while sent or judged, not while its plugin parks it', () => {
  const polled = (r: EntryResult): SubmissionEntry => ({
    ...entry(1, 'polling'),
    submission: r.submission ?? null,
  });
  assert.equal(entryInProgress(entry(1, 'submitting')), true);
  assert.equal(entryInProgress(entry(1, 'polling')), true);
  assert.equal(entryInProgress(polled(result('Running'))), true);
  assert.equal(
    entryInProgress(polled(result('Queued', { parked_by: PARKED }))),
    false,
  );
  assert.equal(
    entryInProgress(
      polled(
        result(null, {
          withheld: { reason: 'plugin_outage', outage: DISABLED },
        }),
      ),
    ),
    false,
  );
  assert.equal(entryInProgress(entry(1, 'done')), false);
});

test('a refusal for a plugin outage asks for the outage to be read again', () => {
  for (const code of [
    'CONTEST_TYPE_DISABLED',
    'CONTEST_TYPE_UNAVAILABLE',
    'PLUGIN_DISABLED',
    'PLUGIN_NOT_LOADED',
    'PLUGIN_UNAVAILABLE',
  ]) {
    assert.equal(isOutageRefusal({ code, message: '' }), true);
  }
  assert.equal(
    isOutageRefusal({ code: 'QUEUE_OVERLOADED', message: '' }),
    false,
  );
  assert.equal(isOutageRefusal(new TypeError('offline')), false);
});
