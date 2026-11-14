import { createContext } from 'react';

import type { LoginRequest, User } from '@/auth';

export interface AuthContextValue {
  user: User | null;
  /** True while the session is being restored on page load. */
  isLoading: boolean;
  /**
   * The server could not be asked whether a session on this browser still
   * holds: nobody is known, neither a user nor a guest, and every request
   * fails unsent. The session keeps asking by itself (`refresh()` asks now)
   * and, once answered, data that failed meanwhile is fetched again.
   */
  unreachable: boolean;
  /**
   * The server ended the session this tab had (not a sign-out): the user is
   * signed out and should be asked to sign in again.
   */
  sessionExpired: boolean;
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
