import { useApiClient } from '@broccoli/web-sdk/api';
import { useQuery } from '@tanstack/react-query';

const REFETCH_INTERVAL_MS = 10_000;

export function useDlqStats({ enabled = true }: { enabled?: boolean } = {}) {
  const apiClient = useApiClient();

  return useQuery({
    queryKey: ['dlq', 'stats'],
    // `/dlq/stats` requires dlq:manage. Callers that render before their own
    // permission check must pass `enabled: false`, or this poll 403-loops for
    // unauthorized viewers (hooks run before an early Unauthorized return).
    enabled,
    refetchInterval: REFETCH_INTERVAL_MS,
    refetchIntervalInBackground: false,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/dlq/stats');
      if (error) throw error;
      return data;
    },
  });
}
