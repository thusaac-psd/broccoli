import { createContext } from 'react';

import type { ApiClient } from '@/api/types';

export type ApiFetch = (
  input: string | URL,
  init?: RequestInit,
) => Promise<Response>;

export interface ApiClientContextValue {
  apiClient: ApiClient;
  /** Authenticated fetch for untyped endpoints (e.g. plugin APIs). */
  apiFetch: ApiFetch;
}

export const ApiClientContext = createContext<ApiClientContextValue | null>(
  null,
);
