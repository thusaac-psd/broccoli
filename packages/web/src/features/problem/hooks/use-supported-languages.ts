import { useApiClient } from '@broccoli/web-sdk/api';
import { useQuery } from '@tanstack/react-query';

import { fetchSupportedLanguages } from '@/features/problem/api/fetch-supported-languages';

/**
 * The languages the server can take work in now, from its registries: a
 * language whose plugin is disabled is not among them. The problem view and
 * its editor share this one query.
 */
export function useSupportedLanguages() {
  const apiClient = useApiClient();
  return useQuery({
    queryKey: ['supported-languages'],
    queryFn: () => fetchSupportedLanguages(apiClient),
    staleTime: 5 * 60 * 1000,
  });
}
