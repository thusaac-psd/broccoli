import { useApiClient } from '@broccoli/web-sdk/api';
import { useQuery } from '@tanstack/react-query';

export const REGISTRATION_STATUS_QUERY_KEY = ['registration-status'] as const;

/** Whether self-registration is open (an admin switches it at runtime). */
export function useRegistrationStatus() {
  const apiClient = useApiClient();
  return useQuery({
    queryKey: REGISTRATION_STATUS_QUERY_KEY,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/auth/registration');
      if (error) throw error;
      return data;
    },
  });
}
