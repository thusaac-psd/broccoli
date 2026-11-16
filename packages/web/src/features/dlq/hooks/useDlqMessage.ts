import { useApiClient } from '@broccoli/web-sdk/api';
import { useQuery } from '@tanstack/react-query';

export function useDlqMessage(id: number | null) {
  const apiClient = useApiClient();

  return useQuery({
    queryKey: ['dlq', 'message', id],
    enabled: id !== null,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/dlq/{id}', {
        params: { path: { id: id! } },
      });
      if (error) throw error;
      return data;
    },
  });
}
