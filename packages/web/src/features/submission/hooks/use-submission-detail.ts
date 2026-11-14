import { useApiClient } from '@broccoli/web-sdk/api';
import { pollInterval } from '@broccoli/web-sdk/hooks';
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
    // Polls until the verdict is final, slower while polls fail.
    refetchInterval: pollInterval<Submission>((submission) =>
      submission && isTerminalStatus(submission.status)
        ? false
        : POLL_INTERVAL_MS,
    ),
  });
}
