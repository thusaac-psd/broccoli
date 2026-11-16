import { useApiClient } from '@broccoli/web-sdk/api';
import { useQuery } from '@tanstack/react-query';

/** The cache key of a contest's config issues. */
export function contestConfigIssuesQueryKey(contestId: number) {
  return ['contest-config-issues', contestId];
}

/**
 * What the plugins find wrong with the contest's configs and its problems'
 * configs in it. Reading them needs `contest:manage`.
 */
export function useContestConfigIssues(
  contestId: number,
  { enabled = true }: { enabled?: boolean } = {},
) {
  const apiClient = useApiClient();
  return useQuery({
    queryKey: contestConfigIssuesQueryKey(contestId),
    queryFn: async () => {
      const { data, error } = await apiClient.GET(
        '/contests/{id}/config/issues',
        { params: { path: { id: contestId } } },
      );
      if (error) throw error;
      return data;
    },
    enabled,
  });
}
