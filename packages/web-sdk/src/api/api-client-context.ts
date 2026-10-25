import { createContext } from 'react';

import type { RefreshedSession } from '@/api/refresh-session';
import type { ApiClient } from '@/api/types';

export type ApiFetch = (
  input: string | URL,
  init?: RequestInit,
) => Promise<Response>;

export interface ApiClientContextValue {
  apiClient: ApiClient;
  /** Authenticated fetch for untyped endpoints (e.g. plugin APIs). */
  apiFetch: ApiFetch;
  /** Sets the in-memory access token used for requests. */
  setAccessToken: (token: string | null) => void;
  /**
   * Exchanges the refresh cookie for a new session (null when it is over).
   * The only way to refresh: concurrent refreshes would rotate the
   * single-use cookie out from under each other.
   */
  refreshSession: () => Promise<RefreshedSession | null>;
}

export const ApiClientContext = createContext<ApiClientContextValue | null>(
  null,
);
