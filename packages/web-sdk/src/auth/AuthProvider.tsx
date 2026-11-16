import { useQueryClient } from '@tanstack/react-query';
import React, {
  useCallback,
  useEffect,
  useMemo,
  useSyncExternalStore,
} from 'react';

import { useApiClient } from '@/api/use-api-client';
import { AuthContext } from '@/auth/auth-context';
import {
  hintsSignedIn,
  nextRefreshInMs,
  sessionEffects,
  sessionHint,
} from '@/auth/session';
import { useSession } from '@/auth/session-context';
import type { LoginRequest } from '@/auth/types';

interface AuthProviderProps {
  children: React.ReactNode;
  sessionStatusKey: string;
}

/**
 * AuthProvider drives the dual-token session the API client owns (see
 * `session.ts`): it bootstraps it on load, renews it in the background, and
 * carries its effects (the cross-tab session hint, cached data) out.
 * Access Token (JWT) lives only in memory, in the session.
 * Refresh Token is stored in an HttpOnly cookie (handled by the browser).
 * Session Hint is stored in localStorage to persist login intent across
 * reloads and to sign every tab out together.
 */
export function AuthProvider({
  children,
  sessionStatusKey,
}: AuthProviderProps) {
  const session = useSession();
  const state = useSyncExternalStore(
    session.subscribe,
    session.getState,
    session.getState,
  );
  const apiClient = useApiClient();
  const queryClient = useQueryClient();

  // Every transition's effects, whoever caused it: this provider, a
  // 401-triggered refresh in the API client, or another tab.
  useEffect(
    () =>
      session.subscribe((next, prev) => {
        const { hint, queries } = sessionEffects(prev, next);
        if (hint !== null) {
          try {
            localStorage.setItem(sessionStatusKey, hint);
          } catch {
            // Storage blocked: the session still works in this tab.
          }
        }
        if (queries === 'clear') queryClient.clear();
        if (queries === 'invalidate') void queryClient.invalidateQueries();
      }),
    [session, sessionStatusKey, queryClient],
  );

  useEffect(() => {
    // Another tab signed out (a removed hint counts as one), or signed in as
    // another user: the shared cookie is theirs now, so renew to switch.
    const handleStorage = (event: StorageEvent) => {
      if (event.key !== sessionStatusKey) return;
      if (!hintsSignedIn(event.newValue)) {
        session.dispatch({ type: 'signed-out' });
        return;
      }
      const state = session.getState();
      if (state.status === 'active' && event.newValue !== sessionHint(state)) {
        void session.refresh();
      }
    };
    window.addEventListener('storage', handleStorage);
    return () => window.removeEventListener('storage', handleStorage);
  }, [session, sessionStatusKey]);

  // Bootstrap. While the server is unreachable the session stays
  // `bootstrapping` and retries, rather than concluding nobody is signed in.
  useEffect(() => {
    if (session.getState().status !== 'bootstrapping') return;
    let hinted = false;
    try {
      hinted = hintsSignedIn(localStorage.getItem(sessionStatusKey));
    } catch {
      // No storage, no hint.
    }
    if (hinted) void session.refresh();
    else session.dispatch({ type: 'no-session' });
  }, [session, sessionStatusKey]);

  // Background renewal (and the bootstrap's retries). Every transition is a
  // new state object, so each one re-arms the timer.
  useEffect(() => {
    const delay = nextRefreshInMs(state);
    if (delay === null) return;
    const timer = setTimeout(() => void session.refresh(), delay);
    return () => clearTimeout(timer);
  }, [session, state]);

  const login = useCallback(
    async (data: LoginRequest) => {
      const { data: resData, error } = await apiClient.POST('/auth/login', {
        body: data,
      });

      if (error) throw new Error(error.message);
      if (!resData) throw new Error('Unexpected login response');

      session.dispatch({ type: 'signed-in', session: resData });
    },
    [apiClient, session],
  );

  const logout = useCallback(
    () =>
      session.signOut(() =>
        // Revokes the refresh cookie; a network error leaves it to expire.
        apiClient.POST('/auth/logout').catch(() => undefined),
      ),
    [apiClient, session],
  );

  const refresh = useCallback(async () => {
    await session.refresh();
  }, [session]);

  const value = useMemo(
    () => ({
      user: state.status === 'active' ? state.user : null,
      accessToken: state.status === 'active' ? state.token : null,
      isLoading: state.status === 'bootstrapping',
      login,
      logout,
      refresh,
    }),
    [state, login, logout, refresh],
  );

  return <AuthContext value={value}>{children}</AuthContext>;
}
