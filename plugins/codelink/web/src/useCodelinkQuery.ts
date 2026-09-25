import { useApiFetch } from '@broccoli/web-sdk/api';
import { useQuery } from '@tanstack/react-query';

import type { ContestInfoResponse } from './types';

export function useCodelinkQuery<T extends ContestInfoResponse>(
  contestId: number | undefined,
  endpoint: 'info' | 'standings',
  autoRefresh = true,
) {
  const apiFetch = useApiFetch();
  return useQuery({
    queryKey: [`codelink-${endpoint}`, contestId],
    enabled: !!contestId,
    queryFn: async (): Promise<T> => {
      const response = await apiFetch(
        `/api/v1/p/codelink/api/plugins/codelink/contests/${contestId}/${endpoint}`,
      );
      if (!response.ok)
        throw new Error(`Codelink ${endpoint}: ${response.status}`);
      return response.json();
    },
    // Polling starts after the server supplies this contest's interval. Zero
    // disables it. Continuing across phase changes includes queued judgements.
    refetchInterval: (query) => {
      const seconds = query.state.data?.scoreboard_refresh_seconds;
      return autoRefresh && seconds ? seconds * 1000 : false;
    },
  });
}
