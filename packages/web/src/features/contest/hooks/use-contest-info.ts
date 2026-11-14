import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuthReady } from '@broccoli/web-sdk/auth';
import { useQuery } from '@tanstack/react-query';

export function useContestInfo(contestId: number) {
  const apiClient = useApiClient();
  const authReady = useAuthReady();
  const query = useQuery({
    queryKey: ['contest', contestId],
    enabled: authReady && Number.isFinite(contestId),
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/contests/{id}', {
        params: { path: { id: contestId } },
      });
      if (error) throw error;
      return data;
    },
  });
  return {
    contest: query.data,
    isLoading: query.isLoading,
    error: query.error,
    query,
  };
}
