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

  const clearSession = useCallback(() => {
    localStorage.setItem(sessionStatusKey, 'false');
    updateAccessToken(null);
    setUser(null);
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
    const data = await refreshApiSession();

    if (!data) {
      clearSession();
      return;
    }

    updateAccessToken(data.token);
    setUser({
      id: data.id,
      username: data.username,
      display_name: data.display_name,
      roles: data.roles,
      permissions: data.permissions,
    });
    localStorage.setItem(sessionStatusKey, 'true');
  }, [refreshApiSession, clearSession, updateAccessToken, sessionStatusKey]);

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
        await refresh();
        // Re-run any queries that fired before the access token was restored
        // and got a 401 because no Authorization header was attached.
        queryClient.invalidateQueries();
      }
      setIsLoading(false);
    };

    initAuth();

    return () => {
      window.removeEventListener(
        AUTH_SESSION_EXPIRED_EVENT,
        handleTokenCleared,
      );
      window.removeEventListener('storage', handleStorage);
    };
  }, [clearSession, refresh, sessionStatusKey, queryClient]);

  // Automatic background refresh loop
  useEffect(() => {
    if (!accessToken) return;

    const timer = setInterval(() => {
      refresh();
    }, REFRESH_INTERVAL_MS);

    return () => clearInterval(timer);
  }, [accessToken, refresh]);

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
