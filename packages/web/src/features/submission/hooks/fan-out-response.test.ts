// Pure-logic test for `pairFanOutSubmissions`/`isWithheldFanOutSubmission`
// (see `fan-out-response.ts`). Runs via Node's built-in test runner + native
// TypeScript support, matching the convention already established by
// `../components/verdict-key.test.ts` and `../components/testCaseDiff.test.ts`:
// `node --experimental-strip-types --test`, no bundler/DOM needed since the
// functions under test are pure.
//
// This pins the fix for a REMAINING ITEM found in code review of the
// submission:rejudge visibility fix: `use-submissions.ts` used to pair
// `POST /admin/submissions/fan-out` responses back to their placeholder
// entries by matching the response's own `target_worker_id` field
// (`created.find((s) => s.target_worker_id === e.targetWorkerId)`). That
// field is legitimately absent when the admin's own Read visibility into a
// just-created submission is denied (e.g. a plugin `Deny` beating the host's
// owner-bypass `Allow` via `Decision::meet`) - the response degrades to the
// bare `{ id }` shape. A field-equality match against an absent field finds
// nothing, silently indistinguishable from "the server didn't return this
// submission at all".
//
// The fix pairs by array index instead: the server returns one response per
// requested `target_worker_id`, in request order, regardless of any
// individual entry's visibility outcome - see `admin_fan_out_submission` in
// `packages/server/src/handlers/submission/rejudge.rs`. Both directions must
// hold: index pairing must still find the right worker for an ordinary
// (Allow) response, and it must correctly flag a Deny response as
// `withheld` instead of quietly mismatching it to the wrong worker or
// dropping it.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { SubmissionAfterMutation } from '@broccoli/web-sdk/submission';

import {
  isWithheldFanOutSubmission,
  pairFanOutSubmissions,
} from './fan-out-response.ts';

function fullSubmission(
  id: number,
  targetWorkerId: string,
): SubmissionAfterMutation {
  return {
    id,
    files: [],
    language: 'cpp',
    status: 'Pending',
    user_id: 1,
    username: 'alice',
    problem_id: 1,
    problem_title: 'Two Sum',
    contest_id: null,
    contest_type: 'icpc',
    judge_epoch: 0,
    target_worker_id: targetWorkerId,
    created_at: '2025-10-01T14:30:00Z',
    result: null,
  } as SubmissionAfterMutation;
}

function withheldSubmission(id: number): SubmissionAfterMutation {
  // Exactly what `apply_filter_to_response_after_mutation` sends on Deny:
  // `serde_json::json!({ "id": id })` - only `id`, nothing else present.
  return { id } as SubmissionAfterMutation;
}

test('isWithheldFanOutSubmission is true for the bare {id} Deny shape', () => {
  assert.equal(isWithheldFanOutSubmission(withheldSubmission(7)), true);
});

test('isWithheldFanOutSubmission is false once any other field is present', () => {
  assert.equal(
    isWithheldFanOutSubmission(fullSubmission(7, 'worker-1')),
    false,
  );
  // Redact keeps the key but nulls the value - still "present", not withheld.
  assert.equal(
    isWithheldFanOutSubmission({
      id: 7,
      files: null,
    } as SubmissionAfterMutation),
    false,
  );
});

test('pairs an all-Allow response to the right worker by request order', () => {
  const targetWorkerIds = ['worker-1', 'worker-2', 'worker-3'];
  const created = [
    fullSubmission(101, 'worker-1'),
    fullSubmission(102, 'worker-2'),
    fullSubmission(103, 'worker-3'),
  ];

  const pairings = pairFanOutSubmissions(targetWorkerIds, created);

  assert.deepEqual(
    pairings.map((p) => [p.workerId, p.submission.id, p.withheld]),
    [
      ['worker-1', 101, false],
      ['worker-2', 102, false],
      ['worker-3', 103, false],
    ],
  );
});

test('a withheld (Deny) entry still pairs to its worker by position, flagged withheld', () => {
  const targetWorkerIds = ['worker-1', 'worker-2', 'worker-3'];
  // worker-2's submission was created (id 102) but the admin's Read
  // visibility into it was denied by a plugin - server sends `{ id: 102 }`.
  const created = [
    fullSubmission(101, 'worker-1'),
    withheldSubmission(102),
    fullSubmission(103, 'worker-3'),
  ];

  const pairings = pairFanOutSubmissions(targetWorkerIds, created);

  assert.deepEqual(
    pairings.map((p) => [p.workerId, p.submission.id, p.withheld]),
    [
      ['worker-1', 101, false],
      ['worker-2', 102, true],
      ['worker-3', 103, false],
    ],
  );
});

test('pairing never trusts the response echo of target_worker_id, even if it lies', () => {
  // Regression guard for the old `.find((s) => s.target_worker_id === ...)`
  // approach: if a response's own `target_worker_id` were ever wrong or
  // stale, field-equality matching would silently mispair. Index pairing
  // must ignore that field entirely for the *pairing* decision - the
  // pairing's `workerId` always comes from the request, not the response.
  const targetWorkerIds = ['worker-1', 'worker-2'];
  const created = [
    fullSubmission(201, 'worker-2'), // wrong `target_worker_id` on purpose
    fullSubmission(202, 'worker-1'), // wrong `target_worker_id` on purpose
  ];

  const pairings = pairFanOutSubmissions(targetWorkerIds, created);

  assert.deepEqual(
    pairings.map((p) => [p.workerId, p.submission.id]),
    [
      ['worker-1', 201],
      ['worker-2', 202],
    ],
  );
});

test('throws if the response length does not match the request length', () => {
  assert.throws(() =>
    pairFanOutSubmissions(
      ['worker-1', 'worker-2'],
      [fullSubmission(1, 'worker-1')],
    ),
  );
});
