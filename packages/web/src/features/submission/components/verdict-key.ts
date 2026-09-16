import type { SubmissionStatus, Verdict } from '@broccoli/web-sdk/submission';
import { isTerminalStatus } from '@broccoli/web-sdk/submission';

export type VerdictKey =
  | 'accepted'
  | 'wrong_answer'
  | 'time_limit'
  | 'memory_limit'
  | 'runtime_error'
  | 'system_error'
  | 'skipped'
  | 'cancelled'
  | 'custom'
  | 'pending';

/**
 * A per-test-case `verdict` of `null`/`undefined` is now ambiguous: it means
 * EITHER "not yet judged" (the submission/judgement hasn't reached this test
 * case yet) OR "judged, but this field was blanked by a visibility field
 * mask" (IOI's `subtask_scores`/`total_only` feedback levels -- see
 * `plugins/ioi/src/feedback.rs`'s `per_test_case_mask_fields`). A mask can
 * only blank a value, never author one, so the wire can no longer
 * distinguish the two cases by verdict alone.
 *
 * `status` (the enclosing submission's or judgement's status, never itself
 * masked by any IOI mask) disambiguates: once a judgement reaches a
 * terminal status (`isTerminalStatus`), every test case it ran has *some*
 * verdict on the wire -- a `null` at that point can only be a redaction, so
 * it renders exactly like the existing authored `"Skipped"` value did
 * pre-refactor (equivalence is defined at the rendered level, not the wire
 * level: a masked test case must be visually indistinguishable from a
 * genuinely skipped one). While the status is still non-terminal, a `null`
 * verdict genuinely means "not run yet".
 *
 * Extracted into its own JSX-free module (rather than living inline in
 * `TestCaseRow.tsx`) so it can be exercised directly by Node's built-in test
 * runner (`node --experimental-strip-types --test`) without needing a
 * JSX-capable loader -- see `verdict-key.test.ts`.
 *
 * Task 17 / Step 2z: `isTerminalStatus` alone is not enough here and must be
 * paired with an explicit `status === 'Running'` check. `test_case_results`
 * is populated exclusively from already-computed DB rows -- there is no
 * placeholder element for a test case that has not run yet -- so a `null`
 * verdict on an element that EXISTS can only ever be a masking artifact,
 * never genuine "not run yet", and that holds at `Running` too (a case can
 * finish and be masked while the judgement as a whole is still `Running`).
 * The backend encodes exactly this: its own guard is
 * `is_terminal() || status == Running` (see
 * `hidden_result_mask_covers_every_list_shape_field_hide_used_to_blank` and
 * sibling tests in `plugins/icpc/src/lib.rs`). Without the `Running` half,
 * a masked test case under a still-`Running` judgement rendered "pending"
 * instead of "skipped" -- the exact bug this predicate was written to fix,
 * reproduced by `isTerminalStatus`'s omission of `Running` (it is
 * deliberately NOT terminal for polling purposes -- see
 * `@broccoli/web-sdk/submission`'s `TERMINAL_STATUSES` -- so that set must
 * not be widened; the extra check belongs here, at the one call site that
 * means something different by "terminal enough to trust a null verdict").
 */
export function getVerdictKey(
  verdict: Verdict | null | undefined,
  status: SubmissionStatus,
): VerdictKey {
  switch (verdict) {
    case 'Accepted':
      return 'accepted';
    case 'WrongAnswer':
      return 'wrong_answer';
    case 'TimeLimitExceeded':
      return 'time_limit';
    case 'MemoryLimitExceeded':
      return 'memory_limit';
    case 'RuntimeError':
      return 'runtime_error';
    case 'SystemError':
      return 'system_error';
    case 'Skipped':
      return 'skipped';
    case 'Cancelled':
      return 'cancelled';
    case null:
    case undefined:
      return isTerminalStatus(status) || status === 'Running'
        ? 'skipped'
        : 'pending';
    default:
      return 'custom';
  }
}
