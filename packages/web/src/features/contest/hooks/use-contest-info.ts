import { useApiClient, useServerTimeReached } from '@broccoli/web-sdk/api';
import { useAuthReady } from '@broccoli/web-sdk/auth';
import { useQuery } from '@tanstack/react-query';
import { useEffect } from 'react';

import { nextPhaseChange } from '@/features/contest/utils/status';

/** How often a contest whose phase is due to change is asked for it again. */
const PHASE_DUE_POLL_MS = 1_000;

/**
 * The contest `contestId`, with the phase the server reports. When the
 * server clock passes the instant the phase next changes, the contest is
 * fetched again until the server reports the new phase, so every view of it
 * moves on together and on the server's word.
 */
export function useContestInfo(contestId: number) {
  const apiClient = useApiClient();
  const authReady = useAuthReady();
  const query = useQuery({
    queryKey: ['contest', contestId],
    enabled: authReady && Number.isFinite(contestId),
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/contests/{id}', {
        params: { path: { id: contestId } },
      });
      if (error) throw error;
      return data;
    },
  });
  const phaseDue = useServerTimeReached(
    query.data ? nextPhaseChange(query.data) : null,
  );
  const { refetch } = query;
  useEffect(() => {
    if (!phaseDue) return;
    void refetch();
    const timer = setInterval(() => void refetch(), PHASE_DUE_POLL_MS);
    return () => clearInterval(timer);
  }, [phaseDue, refetch]);
  return {
    contest: query.data,
    isLoading: query.isLoading,
    error: query.error,
    query,
  };
}
