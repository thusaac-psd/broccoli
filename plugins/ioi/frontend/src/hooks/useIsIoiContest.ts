import { PluginApiError } from '@broccoli/web-sdk/api';
import { useQuery } from '@tanstack/react-query';

import { useIoiApi } from './useIoiApi';

/**
 * Checks if the current contest is IOI-type by fetching
 * the IOI contest info endpoint. If the endpoint succeeds, it's IOI.
 */
export function useIsIoiContest(contestId?: number) {
  const api = useIoiApi();

  const { data, isLoading, error } = useQuery({
    queryKey: ['ioi-contest-info', contestId],
    enabled: !!contestId,
    queryFn: () => api.getContestInfo(contestId!),
    retry: false,
    staleTime: 5 * 60 * 1000,
  });

  const is404 = error instanceof PluginApiError && error.status === 404;
  const isServerError = !!error && !is404;

  return {
    isIoi: !!data,
    contestInfo: data ?? null,
    isLoading,
    error: isServerError ? (error as Error).message : null,
  };
}
