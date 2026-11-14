import { useQuery } from '@tanstack/react-query';

import { useApiClient } from '@/api';
import { pollInterval } from '@/hooks/poll';
import { fetchTestCasePage } from '@/submission/fetch-test-case-page';

export function useTestCasePage({
  submissionId,
  judgementId,
  page = 1,
  testCaseIds,
  resultId,
  fullOutput = false,
  enabled = true,
  live = false,
}: {
  submissionId: number;
  judgementId?: number | null;
  page?: number;
  testCaseIds?: number[];
  resultId?: number;
  fullOutput?: boolean;
  enabled?: boolean;
  live?: boolean;
}) {
  const api = useApiClient();
  const ids = testCaseIds?.join(',');
  return useQuery({
    queryKey: [
      'submission-test-cases',
      submissionId,
      judgementId,
      page,
      ids,
      resultId,
      fullOutput,
      live,
    ],
    enabled: enabled && (!testCaseIds || testCaseIds.length > 0),
    refetchInterval: pollInterval(() => (live ? 1000 : false)),
    queryFn: ({ signal }) =>
      fetchTestCasePage(
        api,
        submissionId,
        judgementId,
        {
          page,
          per_page: 20,
          test_case_ids: ids,
          result_id: resultId,
          full_output: fullOutput,
        },
        signal,
      ),
  });
}
