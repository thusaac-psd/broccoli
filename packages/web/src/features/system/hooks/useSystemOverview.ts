import { useApiClient } from '@broccoli/web-sdk/api';
import { useQuery } from '@tanstack/react-query';

const REFETCH_INTERVAL_MS = 5000;

export function useSystemOverview({
  enabled = true,
}: { enabled?: boolean } = {}) {
  const apiClient = useApiClient();

  return useQuery({
    queryKey: ['system', 'overview'],
    // `/admin/system/overview` is admin-only, so callers without the relevant
    // permission must pass `enabled: false`. Otherwise the query (which polls
    // every REFETCH_INTERVAL_MS) fires for every viewer and 403s on a loop —
    // hooks run before a component's permission-gated early return, so gating
    // the render is not enough; the fetch itself has to be gated here.
    enabled,
    refetchInterval: REFETCH_INTERVAL_MS,
    refetchIntervalInBackground: false,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/admin/system/overview');
      if (error) throw error;
      return data;
    },
  });
}
