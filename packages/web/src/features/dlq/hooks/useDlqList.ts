import { useApiClient } from '@broccoli/web-sdk/api';
import { useQuery } from '@tanstack/react-query';

import type { DlqResolvedFilter } from '@/features/dlq/types';

interface Params {
  page: number;
  perPage: number;
  resolvedFilter: DlqResolvedFilter;
  messageType?: string;
  enabled?: boolean;
}

const REFETCH_INTERVAL_MS = 10_000;

export function useDlqList({
  page,
  perPage,
  resolvedFilter,
  messageType,
  enabled = true,
}: Params) {
  const apiClient = useApiClient();

  return useQuery({
    queryKey: ['dlq', 'list', { page, perPage, resolvedFilter, messageType }],
    // `/dlq` requires dlq:manage. Callers that render before their own
    // permission check must pass `enabled: false`, or this poll 403-loops for
    // unauthorized viewers (hooks run before an early Unauthorized return).
    enabled,
    refetchInterval: REFETCH_INTERVAL_MS,
    refetchIntervalInBackground: false,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/dlq', {
        params: {
          query: {
            page,
            per_page: perPage,
            resolved:
              resolvedFilter === 'all'
                ? undefined
                : resolvedFilter === 'resolved',
            message_type: messageType || undefined,
          },
        },
      });
      if (error) throw error;
      return data;
    },
  });
}
