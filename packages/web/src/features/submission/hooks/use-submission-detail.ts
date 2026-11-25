import { useApiClient } from '@broccoli/web-sdk/api';
import { pollInterval } from '@broccoli/web-sdk/hooks';
import {
  JUDGING_POLL_MS,
  type Submission,
  submissionPollPace,
  submissionViewKeys,
} from '@broccoli/web-sdk/submission';
import { useQuery } from '@tanstack/react-query';

export function useSubmissionDetail(submissionId: number) {
  const apiClient = useApiClient();

  return useQuery<Submission>({
    queryKey: submissionViewKeys.detail(submissionId),
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/submissions/{id}', {
        params: { path: { id: submissionId } },
      });
      if (error) throw error;
      return data;
    },
    // Polls at the submission's pace (`submissionPollPace`) until nothing
    // more can change for this viewer, slower while polls fail; a hidden or
    // gone submission (403, 404) stops it.
    refetchInterval: pollInterval<Submission>((submission) =>
      submission ? submissionPollPace(submission) : JUDGING_POLL_MS,
    ),
  });
}
