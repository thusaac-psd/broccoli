import { useQuery } from '@tanstack/react-query';
import { useCallback } from 'react';

import { useApiClient } from '@/api';

export function useRegistries() {
  const apiClient = useApiClient();

  return useQuery({
    queryKey: ['plugin-registries'],
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/plugins/registries');
      if (error) throw error;
      return data;
    },
    staleTime: 60_000,
  });
}

/**
 * The display name of a language id ("cpp" to "C++"), from the languages
 * the plugins registered; the id itself until they have loaded or when no
 * plugin registers it.
 */
export function useLanguageName(): (id: string) => string {
  const { data } = useRegistries();
  return useCallback(
    (id: string) =>
      data?.languages.find((language) => language.id === id)?.name ?? id,
    [data],
  );
}
