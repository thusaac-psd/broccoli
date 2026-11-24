import { useSubmissionVisibilityChange } from '@broccoli/web-sdk/submission';
import type { QueryClient } from '@tanstack/react-query';

/**
 * The lists that show submissions. Each holds a submission as its viewer may
 * see it, so a change to what the viewer may see reads them again.
 */
const SUBMISSION_LISTS = [
  'problem-recent-submissions',
  'contest-submissions-table',
  'admin-submissions-table',
  'overview-submissions',
] as const;

/**
 * Reads every view of a submission again: its detail, its judgements and
 * the lists that show it. Views that stopped polling because the outcome
 * was hidden pick up what the viewer may see now.
 */
export function rereadSubmissionViews(
  queryClient: QueryClient,
  submissionId: number,
): void {
  for (const key of [
    'submission',
    'submission-judgements',
    'submission-judgement',
  ]) {
    void queryClient.invalidateQueries({ queryKey: [key, submissionId] });
  }
  for (const key of SUBMISSION_LISTS) {
    void queryClient.invalidateQueries({ queryKey: [key] });
  }
}

/**
 * Follows the visibility changes plugins announce
 * (`announceSubmissionVisibilityChange`) for the whole app.
 */
export function useFollowSubmissionVisibility(queryClient: QueryClient): void {
  useSubmissionVisibilityChange((submissionId) =>
    rereadSubmissionViews(queryClient, submissionId),
  );
}
