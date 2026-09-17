import type { SubmissionAfterMutation } from '@broccoli/web-sdk/submission';

/**
 * `POST /admin/submissions/fan-out` returns one `SubmissionAfterMutation`
 * per requested `target_worker_id`, in the SAME ORDER the request sent them:
 * `admin_fan_out_submission` (server `rejudge.rs`) builds `models` from
 * `payload.target_worker_ids` in a single ordered loop and folds `responses`
 * from `models` with no re-sort in between. Index pairing against the
 * request's own `target_worker_ids` is therefore correct regardless of
 * whether the caller's Read visibility into any individual submission is
 * Allow, Redact, or Deny.
 *
 * We deliberately do NOT pair by echoing `target_worker_id` back out of the
 * response (as earlier code did via `Array.find`): on a Deny decision the
 * response degrades to the bare `{ id }` shape (see
 * `SubmissionResponseAfterMutation`'s doc comment server-side,
 * `packages/server/src/models/submission.rs`), so `target_worker_id` is
 * `undefined` there and a field-equality match finds nothing - not "this
 * submission exists but its worker assignment isn't disclosed to you", just
 * silently missing, which read as `NO_MATCH` even though the submission was
 * in fact created. The caller already knows which worker each response slot
 * targets - it's their own request - so index pairing recovers that mapping
 * without depending on the server repeating a field it may legitimately
 * decline to disclose.
 */
export interface FanOutPairing {
  workerId: string;
  submission: SubmissionAfterMutation;
  /**
   * True when the server created this submission but the caller's own Read
   * decision for it was Deny. The mutation happened - `submission.id` is
   * real - but nothing else about it is visible to this caller, so there is
   * nothing to render and no point polling `GET /submissions/{id}` for it:
   * that endpoint degrades the same Deny decision to a 404 (unlike this
   * mutation response), so it would just fail forever.
   */
  withheld: boolean;
}

/**
 * A withheld (Deny) fan-out response is *exactly* `{ id }` - every other
 * field is absent from the JSON object, not present-and-`null`. That is the
 * one bit of information the server is willing to leak through this shape:
 * whether the viewer's Read visibility into a submission they just caused to
 * be created is Deny. Redact and Allow both keep every key, so this check
 * only trips on genuine Deny.
 */
export function isWithheldFanOutSubmission(
  submission: SubmissionAfterMutation,
): boolean {
  return Object.keys(submission).length <= 1;
}

export function pairFanOutSubmissions(
  targetWorkerIds: readonly string[],
  created: readonly SubmissionAfterMutation[],
): FanOutPairing[] {
  if (created.length !== targetWorkerIds.length) {
    throw new Error(
      `Fan-out response returned ${created.length} submission(s) for ${targetWorkerIds.length} requested worker(s)`,
    );
  }

  return targetWorkerIds.map((workerId, index) => {
    // Safe: the length check above guarantees `created[index]` is in range.
    const submission = created[index] as SubmissionAfterMutation;
    return {
      workerId,
      submission,
      withheld: isWithheldFanOutSubmission(submission),
    };
  });
}
