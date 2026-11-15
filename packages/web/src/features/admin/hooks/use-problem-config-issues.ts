import { useApiClient } from '@broccoli/web-sdk/api';
import { useQuery } from '@tanstack/react-query';

import { problemConfigIssuesQueryKey } from '@/features/admin/hooks/test-case-queries';

/**
 * The problem's stored configs that their plugins now refuse. Reading them
 * needs `problem:edit`, so callers without it pass `enabled: false`.
 */
export function useProblemConfigIssues(
  problemId: number,
  { enabled = true }: { enabled?: boolean } = {},
) {
  const apiClient = useApiClient();
  return useQuery({
    queryKey: problemConfigIssuesQueryKey(problemId),
    queryFn: async () => {
      const { data, error } = await apiClient.GET(
        '/problems/{id}/config/issues',
        { params: { path: { id: problemId } } },
      );
      if (error) throw error;
      return data;
    },
    enabled,
  });
}
