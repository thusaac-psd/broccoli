import {
  submissionViewKeys,
  useSubmissionVisibilityChange,
} from '@broccoli/web-sdk/submission';
import type { QueryClient } from '@tanstack/react-query';

/**
 * Reads every view of a submission again, after its visibility changed: its detail, its judgements, its
 * test case rows and the lists that show it, all under
 * `submissionViewKeys.all`. Views that stopped polling because the outcome
 * was hidden, or that hold what the viewer could see before, pick up what
 * the viewer may see now. A list may show the submission whatever its
 * filters, so the change reads every view rather than only this
 * submission's.
 */
export function rereadSubmissionViews(queryClient: QueryClient): void {
  void queryClient.invalidateQueries({ queryKey: submissionViewKeys.all });
}

/**
 * Follows the visibility changes plugins announce
 * (`announceSubmissionVisibilityChange`) for the whole app.
 */
export function useFollowSubmissionVisibility(queryClient: QueryClient): void {
  useSubmissionVisibilityChange(() => rereadSubmissionViews(queryClient));
}
