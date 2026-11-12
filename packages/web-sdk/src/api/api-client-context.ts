import { createContext } from 'react';

import type { RefreshOutcome } from '@/api/refresh-session';
import type { ApiClient } from '@/api/types';

export type ApiFetch = (
  input: string | URL,
  init?: RequestInit,
) => Promise<Response>;

export interface ApiClientContextValue {
  apiClient: ApiClient;
  /** The absolute API base URL every request is resolved against. */
  baseUrl: string;
  /** Authenticated fetch for untyped endpoints (e.g. plugin APIs). */
  apiFetch: ApiFetch;
  /** Sets the in-memory access token used for requests. */
  setAccessToken: (token: string | null) => void;
  /**
   * Exchanges the refresh cookie for a new session. The only way to refresh:
   * concurrent refreshes would rotate the single-use cookie out from under
   * each other. Only a `rejected` outcome means the session is over.
   */
  refreshSession: () => Promise<RefreshOutcome>;
}

export const ApiClientContext = createContext<ApiClientContextValue | null>(
  null,
);
