import { useApiClient } from '@broccoli/web-sdk/api';
import { useSessionView } from '@broccoli/web-sdk/auth';
import {
  isTerminalStatus,
  type Submission,
} from '@broccoli/web-sdk/submission';
import { useQuery } from '@tanstack/react-query';

import {
  isPermanentPollFailure,
  POLL_INTERVAL_MS,
} from '@/features/submission/hooks/submission-entries';

export function useSubmissionDetail(submissionId: number) {
  const apiClient = useApiClient();
  const session = useSessionView();

  return useQuery<Submission>({
    queryKey: ['submission', submissionId],
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/submissions/{id}', {
        params: { path: { id: submissionId } },
      });
      if (error) throw error;
      return data;
    },
    // Polls until the verdict is final, or until a failure asking again
    // cannot change (the submission is gone or hidden). A blip or a 502 only
    // delays the next poll.
    refetchInterval: (query) => {
      const status = query.state.data?.status;
      if (status && isTerminalStatus(status)) return false;
      const failed = query.state.error as { status?: number } | null;
      if (
        failed &&
        isPermanentPollFailure(
          failed.status ?? null,
          session.getState().status === 'signedOut',
        )
      ) {
        return false;
      }
      return POLL_INTERVAL_MS;
    },
  });
}
