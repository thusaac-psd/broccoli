import { useApiClient } from '@broccoli/web-sdk/api';
import { useQuery } from '@tanstack/react-query';

export const REGISTRATION_STATUS_QUERY_KEY = ['registration-status'] as const;

/**
 * Whether self-registration is open (an admin switches it at runtime). Until
 * the answer arrives it is unknown, so a page that offers signing up waits
 * for `open: true` (see `useRegistrationOpen`).
 */
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

/**
 * Whether to offer signing up: only once the server says registration is
 * open, so a closed server never shows a link that leads to "closed".
 */
export function useRegistrationOpen(): boolean {
  return useRegistrationStatus().data?.open === true;
}
