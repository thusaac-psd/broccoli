import { useAuth, useAuthReady, viewerCacheKey } from '@broccoli/web-sdk/auth';
import { pollInterval } from '@broccoli/web-sdk/hooks';
import { useQuery } from '@tanstack/react-query';

import { liveStreamKey, useLiveStream } from '../lib/live-stream';
import { useBracketApi } from './useBracketApi';

/**
 * Share the same visibility-scoped snapshot across contest and management
 * views. While the live stream is connected it keeps this query current, so
 * the query polls only while it is not.
 */
export function useBracket(
  contestId: number | undefined,
  { enabled = true, pollEveryMs = 5_000 } = {},
) {
  const { user } = useAuth();
  const authReady = useAuthReady();
  const api = useBracketApi();
  const viewerKey = viewerCacheKey(user);
  const streamed = useLiveStream(
    contestId ? liveStreamKey(contestId, viewerKey) : null,
  );
  return useQuery({
    queryKey: ['codelink-bracket-bracket', contestId, viewerKey],
    enabled: authReady && !!contestId && enabled,
    queryFn: () => api.getBracket(contestId as number),
    refetchInterval: pollInterval(() => (streamed ? false : pollEveryMs)),
  });
}
