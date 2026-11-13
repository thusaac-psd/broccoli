import { useQuery } from '@tanstack/react-query';

import { ApiError, useIcpcApi } from './useIcpcApi';

/**
 * Checks if the current contest is ICPC-type by fetching
 * the ICPC contest info endpoint. If the endpoint succeeds, it's ICPC.
 */
export function useIsIcpcContest(contestId?: number) {
  const api = useIcpcApi();

  const { data, isLoading, error } = useQuery({
    queryKey: ['icpc-contest-info', contestId],
    enabled: !!contestId,
    queryFn: () => api.getContestInfo(contestId!),
    retry: false,
    staleTime: 5 * 60 * 1000,
  });

  const is404 = error instanceof ApiError && error.status === 404;
  const isServerError = !!error && !is404;

  return {
    isIcpc: !!data,
    contestInfo: data ?? null,
    isLoading,
    error: isServerError ? (error as Error).message : null,
  };
}
