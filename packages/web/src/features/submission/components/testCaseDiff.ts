// Pure diff logic for comparing a historical judgement's per-test-case result
// against the current judgement's result for the same test case, used by
// `SubmissionJudgementHistory.tsx`'s `TestCaseDiffNote` / `JudgementDeltaSummary`.
//
// `TestCaseResultResponse.verdict` / `.score` are `Option`-widened on the wire
// (see `packages/server/src/models/submission.rs`): a visibility `FieldMask`
// (ICPC freeze, IOI `subtask_scores`/`total_only` feedback levels) blanks
// them to JSON `null` for a viewer who isn't entitled to the per-test-case
// breakdown. Because the kernel memoises its decision per `(Action,
// Resource)`, masking is uniform across every judgement version of the same
// submission shown to the same viewer in the same request -- so a masked
// field reads as `null` on BOTH the historical and the current test case.
//
// A naive `a !== b` comparison therefore silently reports "unchanged"
// (`null !== null` is `false`) for a field we cannot actually observe. That
// is wrong: "we cannot tell whether this changed" is a different fact from
// "this did not change", and collapsing the two means a rejudge that
// genuinely flipped a masked verdict/score never surfaces a diff badge to a
// viewer entitled to see that a change happened, even if not what it was.
//
// `diffNullableField` keeps those three outcomes distinct. `same`/`different`
// are only returned when both sides are actually observable; `unknown` is
// returned when both sides are masked. The one remaining case -- exactly one
// side masked -- is treated as `different`: the two responses are not
// byte-identical (one carries a value, the other doesn't), so claiming
// "unchanged" would be equally wrong, and this can only happen for the
// SAME viewer if the underlying value differs (masking is otherwise uniform
// per submission/request as described above).
export type FieldDiff = 'same' | 'different' | 'unknown';

export function diffNullableField<T>(
  value: T | null | undefined,
  current: T | null | undefined,
): FieldDiff {
  const valueMasked = value == null;
  const currentMasked = current == null;

  if (valueMasked && currentMasked) return 'unknown';
  if (valueMasked !== currentMasked) return 'different';
  return value === current ? 'same' : 'different';
}

/** The subset of `TestCaseResultResponse` the diff cares about. */
export interface DiffableTestCase {
  verdict?: string | null;
  score?: number | null;
  time_used?: number | null;
  memory_used?: number | null;
  checker_output?: string | null;
}

export type CaseDiffStatus = 'changed' | 'unknown' | 'unchanged';

/**
 * Combines the per-field diffs for one test case into a single verdict:
 * - `changed` if any field is observably different.
 * - `unknown` if no field is observably different, but at least one field
 *   could not be compared because both sides are masked.
 * - `unchanged` only when every field was observable and identical.
 *
 * `changed` takes priority over `unknown`: if we can already prove a
 * difference from an unmasked field, an unrelated masked field doesn't
 * weaken that conclusion.
 */
export function testCaseDiffStatus(
  testCase: DiffableTestCase,
  currentTestCase: DiffableTestCase,
): CaseDiffStatus {
  const diffs: FieldDiff[] = [
    diffNullableField(testCase.verdict, currentTestCase.verdict),
    diffNullableField(testCase.score, currentTestCase.score),
    diffNullableField(testCase.time_used, currentTestCase.time_used),
    diffNullableField(testCase.memory_used, currentTestCase.memory_used),
    diffNullableField(testCase.checker_output, currentTestCase.checker_output),
  ];

  if (diffs.includes('different')) return 'changed';
  if (diffs.includes('unknown')) return 'unknown';
  return 'unchanged';
}
