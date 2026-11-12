import { useApiClient } from '@broccoli/web-sdk/api';
import {
  isTerminalStatus,
  type Submission,
} from '@broccoli/web-sdk/submission';
import { useQuery } from '@tanstack/react-query';

import { POLL_INTERVAL_MS } from '@/features/submission/hooks/submission-entries';

export function useSubmissionDetail(submissionId: number) {
  const apiClient = useApiClient();

  const {
    data: submission,
    isLoading,
    error,
  } = useQuery<Submission>({
    queryKey: ['submission', submissionId],
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/submissions/{id}', {
        params: { path: { id: submissionId } },
      });
      if (error) throw error;
      return data;
    },
    // Polls until the verdict is final. A failure that outlasted the query's
    // retries (the submission is gone or hidden) stops it too.
    refetchInterval: (query) => {
      const status = query.state.data?.status;
      if (status && isTerminalStatus(status)) return false;
      if (query.state.status === 'error') return false;
      return POLL_INTERVAL_MS;
    },
  });

  return { submission: submission ?? null, isLoading, error };
}
