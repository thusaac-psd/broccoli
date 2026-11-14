import { useQuery } from '@tanstack/react-query';

import { useIoiApi } from './useIoiApi';

/**
 * Checks if the current contest is IOI-type by fetching
 * the IOI contest info endpoint. If the endpoint succeeds, it's IOI.
 */
export function useIsIoiContest(contestId?: number) {
  const api = useIoiApi();

  // Answers whether the contest is this plugin's: a failure means it is not,
  // and the slot shows the host's own view.
  // eslint-disable-next-line no-restricted-syntax
  const { data, isLoading } = useQuery({
    queryKey: ['ioi-contest-info', contestId],
    enabled: !!contestId,
    queryFn: () => api.getContestInfo(contestId!),
    retry: false,
    staleTime: 5 * 60 * 1000,
  });

  return {
    isIoi: !!data,
    contestInfo: data ?? null,
    isLoading,
  };
}
