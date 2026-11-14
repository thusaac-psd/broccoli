import { useQuery } from '@tanstack/react-query';

import { useIcpcApi } from './useIcpcApi';

/**
 * Checks if the current contest is ICPC-type by fetching
 * the ICPC contest info endpoint. If the endpoint succeeds, it's ICPC.
 */
export function useIsIcpcContest(contestId?: number) {
  const api = useIcpcApi();

  // Answers whether the contest is this plugin's: a failure means it is not,
  // and the slot shows the host's own view.
  // eslint-disable-next-line no-restricted-syntax
  const { data, isLoading } = useQuery({
    queryKey: ['icpc-contest-info', contestId],
    enabled: !!contestId,
    queryFn: () => api.getContestInfo(contestId!),
    retry: false,
    staleTime: 5 * 60 * 1000,
  });

  return {
    isIcpc: !!data,
    contestInfo: data ?? null,
    isLoading,
  };
}
