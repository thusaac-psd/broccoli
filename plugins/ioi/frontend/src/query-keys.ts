import { submissionViewKeys } from '@broccoli/web-sdk/submission';

/**
 * The keys of IOI's queries whose answers depend on what the viewer may see:
 * a token unlocks a submission's outcome and moves the board, which scores a
 * contestant's best tokened submission. They sit under the host's submission
 * views, so a token spent in any tab reads every one of them again.
 */
export const ioiKeys = {
  tokenStatus: (contestId: number | null | undefined) =>
    submissionViewKeys.plugin('ioi', 'token-status', contestId),
  subtaskScores: (
    contestId: number | null | undefined,
    submissionId: number | null | undefined,
    ...judged: unknown[]
  ) =>
    submissionViewKeys.plugin(
      'ioi',
      'subtask-scores',
      contestId,
      submissionId,
      ...judged,
    ),
  scoreboard: (contestId: number | null | undefined) =>
    submissionViewKeys.plugin('ioi', 'scoreboard', contestId),
};
