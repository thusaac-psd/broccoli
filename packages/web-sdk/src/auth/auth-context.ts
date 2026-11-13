import { createContext } from 'react';

import type { LoginRequest, User } from '@/auth';

export interface AuthContextValue {
  user: User | null;
  /** True while the session is being restored on page load. */
  isLoading: boolean;
  /**
   * The current access token (JWT), kept in JS memory. Renewed in place by
   * every refresh, including one a 401 triggers.
   */
  accessToken: string | null;
  login: (data: LoginRequest) => Promise<void>;
  logout: () => Promise<void>;
  refresh: () => Promise<void>;
}

export const AuthContext = createContext<AuthContextValue | null>(null);
