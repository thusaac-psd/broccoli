// Pure ranking-validation logic for the Ordering view. Mirrors
// `is_permutation_of` in plugins/codelink-bracket/src/ordering.rs exactly:
// same algorithm (sort both sides, compare), same definition of "valid"
// (every element of `group`, each exactly once; order of `group` itself is
// irrelevant). Kept in lockstep so a ranking the UI lets the player submit
// can never be one `record_order` on the server rejects.
//
// The backend rejects an invalid `order` with a 400 either way -- this
// module exists so the UI can refuse to even attempt the doomed request,
// per this task's requirement that a non-permutation be unsubmittable, not
// merely rejected after a round trip.

/**
 * Whether `order` contains exactly the same values as `group`, each exactly
 * once. Both arrays are expected to have the same fixed length (3, for this
 * plugin's problem groups), but the check itself does not assume that --  a
 * length mismatch alone is enough to fail.
 */
export function isPermutationOf(
  order: readonly number[],
  group: readonly number[],
): boolean {
  if (order.length !== group.length) {
    return false;
  }
  const sortedOrder = [...order].sort((a, b) => a - b);
  const sortedGroup = [...group].sort((a, b) => a - b);
  return sortedOrder.every((value, index) => value === sortedGroup[index]);
}

/**
 * Whether a ranking-in-progress is complete and valid enough to submit: it
 * must be a full-length permutation of the opponent's group. A partial
 * ranking (the player has picked 1 or 2 of the 3 problems so far) is never
 * submittable -- only "all 3, no repeats, no omissions" is.
 */
export function canSubmitOrder(
  order: readonly number[],
  group: readonly (number | null)[],
): boolean {
  const resolvedGroup: number[] = [];
  for (const problemId of group) {
    if (problemId === null) {
      // The opponent's group is not fully visible to this viewer yet -- there
      // is nothing valid to rank against, regardless of what `order` contains.
      return false;
    }
    resolvedGroup.push(problemId);
  }
  return isPermutationOf(order, resolvedGroup);
}
