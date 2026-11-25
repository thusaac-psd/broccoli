import { queryOptions, useQuery } from '@tanstack/react-query';
import { useCallback } from 'react';

import { type ApiClient, useApiClient } from '@/api';
import type { components } from '@/api/schema';

/** What the plugins registered: contest types, languages, and the rest. */
export type Registries = components['schemas']['RegistriesResponse'];

/**
 * The one query of what the plugins registered (`GET /plugins/registries`):
 * every reader shares its key, so a page asks for it once, and anything that
 * changes what is registered (a plugin enabled or disabled, a submit refused
 * for one) invalidates this key.
 */
export const REGISTRIES_QUERY_KEY = ['plugin-registries'] as const;

/** The options of the registries query, for a reader that selects from it. */
export function registriesQuery(apiClient: ApiClient) {
  return queryOptions({
    queryKey: REGISTRIES_QUERY_KEY,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/plugins/registries');
      if (error) throw error;
      return data satisfies Registries;
    },
    staleTime: 60_000,
  });
}

export function useRegistries() {
  return useQuery(registriesQuery(useApiClient()));
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
