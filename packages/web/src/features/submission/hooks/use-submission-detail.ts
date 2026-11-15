import { useApiClient } from '@broccoli/web-sdk/api';
import { POLL_BACKOFF_MAX_MS, pollInterval } from '@broccoli/web-sdk/hooks';
import {
  isTerminalStatus,
  type Submission,
} from '@broccoli/web-sdk/submission';
import { useQuery } from '@tanstack/react-query';

import { POLL_INTERVAL_MS } from '@/features/submission/hooks/submission-entries';

export function useSubmissionDetail(submissionId: number) {
  const apiClient = useApiClient();

  return useQuery<Submission>({
    queryKey: ['submission', submissionId],
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/submissions/{id}', {
        params: { path: { id: submissionId } },
      });
      if (error) throw error;
      return data;
    },
    // Polls until the verdict is final for this viewer (a withheld one is,
    // `isTerminalStatus(null)`), slower while polls fail; a hidden or gone
    // submission (403, 404) stops it. One waiting for its contest's plugin
    // changes only once an administrator acts, so it is asked for rarely.
    refetchInterval: pollInterval<Submission>((submission) =>
      submission && isTerminalStatus(submission.status)
        ? false
        : submission?.plugin_outage
          ? POLL_BACKOFF_MAX_MS
          : POLL_INTERVAL_MS,
    ),
  });
}
