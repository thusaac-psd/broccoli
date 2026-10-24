import { useAuth, useAuthReady, viewerCacheKey } from '@broccoli/web-sdk/auth';
import { useQuery } from '@tanstack/react-query';

import { useBracketApi } from './useBracketApi';

/** Share the same visibility-scoped snapshot across contest and management views. */
export function useBracket(
  contestId: number | undefined,
  { enabled = true, refetchInterval = 5_000 } = {},
) {
  const { user } = useAuth();
  const authReady = useAuthReady();
  const api = useBracketApi();
  return useQuery({
    queryKey: ['codelink-bracket-bracket', contestId, viewerCacheKey(user)],
    enabled: authReady && !!contestId && enabled,
    queryFn: () => api.getBracket(contestId as number),
    refetchInterval,
  });
}
