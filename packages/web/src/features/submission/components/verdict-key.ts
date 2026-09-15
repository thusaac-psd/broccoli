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
      return isTerminalStatus(status) ? 'skipped' : 'pending';
    default:
      return 'custom';
  }
}
