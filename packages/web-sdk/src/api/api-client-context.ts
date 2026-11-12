import { createContext } from 'react';

import type { ApiClient } from '@/api/types';
import type { Session } from '@/auth/session';

export type ApiFetch = (
  input: string | URL,
  init?: RequestInit,
) => Promise<Response>;

export interface ApiClientContextValue {
  apiClient: ApiClient;
  /** Authenticated fetch for untyped endpoints (e.g. plugin APIs). */
  apiFetch: ApiFetch;
  /**
   * The sign-in session every request takes its access token from. Its
   * `refresh` is the only way to refresh: concurrent refreshes would rotate
   * the single-use cookie out from under each other.
   */
  session: Session;
}

export const ApiClientContext = createContext<ApiClientContextValue | null>(
  null,
);
