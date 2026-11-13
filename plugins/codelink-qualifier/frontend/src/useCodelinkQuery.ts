import { usePluginApi } from '@broccoli/web-sdk/api';
import { useQuery } from '@tanstack/react-query';

import type { ContestInfoResponse } from './types';

export function useCodelinkQuery<T extends ContestInfoResponse>(
  contestId: number | undefined,
  endpoint: 'info' | 'standings',
  autoRefresh = true,
) {
  const api = usePluginApi('codelink-qualifier');
  return useQuery({
    queryKey: [`codelink-qualifier-${endpoint}`, contestId],
    enabled: !!contestId,
    queryFn: () =>
      api.get<T>(
        `/api/plugins/codelink-qualifier/contests/${contestId}/${endpoint}`,
      ),
    // Polling starts after the server supplies this contest's interval. Zero
    // disables it. Continuing across phase changes includes queued judgements.
    refetchInterval: (query) => {
      const seconds = query.state.data?.scoreboard_refresh_seconds;
      return autoRefresh && seconds ? seconds * 1000 : false;
    },
  });
}
