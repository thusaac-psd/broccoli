import { serverNow, useApiClient } from '@broccoli/web-sdk/api';
import { useAuthReady } from '@broccoli/web-sdk/auth';
import { type Contest, contestPollPace } from '@broccoli/web-sdk/contest';
import { pollInterval } from '@broccoli/web-sdk/hooks';
import { useQuery } from '@tanstack/react-query';

/** The query key of the contest `contestId`, for refetching it from elsewhere. */
export const contestQueryKey = (contestId: number) => ['contest', contestId];

/**
 * The contest `contestId`, with the phase and plugin outage the server
 * reports. The query polls itself (`contestPollPace`): once the server clock
 * passes the instant the phase next changes, until the server reports the
 * new phase; about once a minute while the contest runs, so an outage that
 * starts or ends shows; and slowly while the contest's plugin is out, until
 * it is back.
 * Every view of the contest shares this one query, so they move on together
 * and on the server's word, and a poll still in flight is never cancelled
 * for the next.
 */
export function useContestInfo(contestId: number) {
  const apiClient = useApiClient();
  const authReady = useAuthReady();
  const query = useQuery({
    queryKey: contestQueryKey(contestId),
    enabled: authReady && Number.isFinite(contestId),
    queryFn: async ({ signal }) => {
      const { data, error } = await apiClient.GET('/contests/{id}', {
        params: { path: { id: contestId } },
        signal,
      });
      if (error) throw error;
      return data;
    },
    refetchInterval: pollInterval<Contest>((contest) =>
      contest ? contestPollPace(contest, serverNow()) : false,
    ),
  });
  return {
    contest: query.data,
    isLoading: query.isLoading,
    error: query.error,
    query,
  };
}
