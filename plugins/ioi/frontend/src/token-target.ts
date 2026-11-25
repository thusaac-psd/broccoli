import type { TokenStatusResponse } from './types';

/**
 * The submission a token would be spent on: the one the page shows when the
 * plugin lists it as usable, or else the newest usable one of this problem
 * (the list runs newest first), so a submission still judging or already
 * tokened never hides an older one a token can unlock. Only the plugin's
 * list can say: feedback may withhold a submission's status from its owner.
 */
export function tokenTarget(
  usable: TokenStatusResponse['usable_submissions'],
  problemId: number | undefined,
  shownId: number | undefined,
): number | undefined {
  const ofProblem = usable.filter((u) => u.problem_id === problemId);
  return (ofProblem.find((u) => u.submission_id === shownId) ?? ofProblem[0])
    ?.submission_id;
}
