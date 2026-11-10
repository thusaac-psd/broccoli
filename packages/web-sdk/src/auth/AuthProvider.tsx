import { useQueryClient } from '@tanstack/react-query';
import React, {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';

import { useApiClient, useRefreshSession, useSetApiAccessToken } from '@/api';
import {
  AUTH_SESSION_EXPIRED_EVENT,
  type LoginRequest,
  type User,
} from '@/auth';
import { AuthContext } from '@/auth/auth-context';

// Refresh the access token every 4.5 minutes
const REFRESH_INTERVAL_MS = 4.5 * 60 * 1000;

interface AuthProviderProps {
  children: React.ReactNode;
  sessionStatusKey: string;
}

/**
 * AuthProvider manages the dual-token authentication lifecycle.
 * Access Token (JWT) is stored in memory and synced with ApiClientProvider.
 * Refresh Token is stored in an HttpOnly cookie (handled by the browser).
 * Session Hint is stored in localStorage to persist login intent across reloads.
 */
export function AuthProvider({
  children,
  sessionStatusKey,
}: AuthProviderProps) {
  const [user, setUser] = useState<User | null>(null);
  const [accessToken, setAccessToken] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const bootstrapStartedRef = useRef(false);
  // Whether the bootstrap refresh has settled (the server answered).
  const settledRef = useRef(false);
  // When the next background refresh runs: a full interval after a success,
  // sooner (with backoff) while the server is unreachable, never once the
  // session is over. A fresh object re-arms the timer even for the same delay.
  const [nextRefresh, setNextRefresh] = useState<{ inMs: number } | null>(null);

  const apiClient = useApiClient();
  const setApiAccessToken = useSetApiAccessToken();
  const refreshApiSession = useRefreshSession();

  const queryClient = useQueryClient();

  const updateAccessToken = useCallback(
    (token: string | null) => {
      setAccessToken(token);
      setApiAccessToken(token);
    },
    [setApiAccessToken],
  );

  // Ends the session in this tab and, through the session hint, in every
  // other tab. Only for an explicit logout or a definitive auth rejection:
  // anything weaker would sign out tabs whose session is fine.
  const clearSession = useCallback(() => {
    localStorage.setItem(sessionStatusKey, 'false');
    updateAccessToken(null);
    setUser(null);
    setNextRefresh(null);
    settledRef.current = true;
    setIsLoading(false);
    queryClient.clear();
  }, [updateAccessToken, sessionStatusKey, queryClient]);

  const logout = useCallback(async () => {
    // Notify backend to revoke the refresh token
    await apiClient.POST('/auth/logout').catch(() => {
      // Ignore network errors on logout
    });
    clearSession();
  }, [apiClient, clearSession]);

  // Shares the API client's refresh, so this background refresh can never race
  // a 401-triggered one for the single-use refresh cookie.
  const refresh = useCallback(async () => {
    const outcome = await refreshApiSession();

    if (outcome.status === 'unavailable') {
      // Offline, a gateway 502, a database blip: the session is probably
      // fine, so keep it (and keep the bootstrap pending) and ask again.
      setNextRefresh({ inMs: outcome.retryInMs });
      return;
    }

    if (outcome.status === 'rejected') {
      clearSession();
      return;
    }

    const data = outcome.session;
    updateAccessToken(data.token);
    setUser({
      id: data.id,
      username: data.username,
      display_name: data.display_name,
      roles: data.roles,
      permissions: data.permissions,
    });
    localStorage.setItem(sessionStatusKey, 'true');
    setNextRefresh({ inMs: REFRESH_INTERVAL_MS });

    if (!settledRef.current) {
      settledRef.current = true;
      setIsLoading(false);
      // Re-run any queries that fired before the access token was restored
      // and got a 401 because no Authorization header was attached.
      queryClient.invalidateQueries();
    }
  }, [
    refreshApiSession,
    clearSession,
    updateAccessToken,
    sessionStatusKey,
    queryClient,
  ]);

  const login = useCallback(
    async (data: LoginRequest) => {
      const { data: resData, error } = await apiClient.POST('/auth/login', {
        body: data,
      });

      if (error) throw new Error(error.message);
      if (!resData) throw new Error('Unexpected login response');

      updateAccessToken(resData.token);
      setUser({
        id: resData.id,
        username: resData.username,
        display_name: resData.display_name,
        roles: resData.roles,
        permissions: resData.permissions,
      });

      localStorage.setItem(sessionStatusKey, 'true');
      setNextRefresh({ inMs: REFRESH_INTERVAL_MS });
      queryClient.clear();
    },
    [apiClient, updateAccessToken, sessionStatusKey, queryClient],
  );

  // Sync state with other tabs or infrastructure events
  useEffect(() => {
    const handleTokenCleared = (event: Event) => {
      const key = (event as CustomEvent<{ key?: string }>).detail?.key;
      // If no key provided, clear all. Otherwise, check if it matches our session hint key.
      if (!key || key === sessionStatusKey) {
        clearSession();
      }
    };

    const handleStorage = (event: StorageEvent) => {
      // Another tab signalled logout: `clearSession` writes the string 'false'
      // (and a removal yields null), so treat anything that is not 'true' as
      // logged-out. Checking only for null missed the 'false' write, leaving
      // sibling tabs authenticated after a logout on a shared machine.
      if (event.key === sessionStatusKey && event.newValue !== 'true') {
        clearSession();
      }
    };

    window.addEventListener(AUTH_SESSION_EXPIRED_EVENT, handleTokenCleared);
    window.addEventListener('storage', handleStorage);

    const initAuth = async () => {
      if (bootstrapStartedRef.current) return;
      bootstrapStartedRef.current = true;

      const sessionHint = localStorage.getItem(sessionStatusKey);
      if (sessionHint === 'true') {
        // Settles the bootstrap once the server has answered either way;
        // while it is unreachable the app keeps waiting instead of
        // concluding the user is signed out.
        await refresh();
      } else {
        settledRef.current = true;
        setIsLoading(false);
      }
    };

    initAuth();

    return () => {
      window.removeEventListener(
        AUTH_SESSION_EXPIRED_EVENT,
        handleTokenCleared,
      );
      window.removeEventListener('storage', handleStorage);
    };
  }, [clearSession, refresh, sessionStatusKey]);

  // Automatic background refresh loop
  useEffect(() => {
    if (!nextRefresh) return;
    const timer = setTimeout(() => void refresh(), nextRefresh.inMs);
    return () => clearTimeout(timer);
  }, [nextRefresh, refresh]);

  const value = useMemo(
    () => ({
      user,
      accessToken,
      isLoading,
      login,
      logout,
      refresh,
    }),
    [user, accessToken, isLoading, login, logout, refresh],
  );

  return <AuthContext value={value}>{children}</AuthContext>;
}
