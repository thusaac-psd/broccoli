import createClient from 'openapi-fetch';
import { type ReactNode, useCallback, useMemo, useRef } from 'react';

import { ApiClientContext } from '@/api/api-client-context';
import { refreshSession as refreshSharedSession } from '@/api/refresh-session';
import type { paths } from '@/api/schema';
import { AUTH_SESSION_EXPIRED_EVENT } from '@/auth/types';

interface ApiClientProviderProps {
  children: ReactNode;
  baseUrl: string;
}

async function isTokenInvalidResponse(response: Response): Promise<boolean> {
  if (response.status !== 401) {
    return false;
  }

  try {
    const body = await response.clone().json();
    // Only react when the token is explicitly invalid/expired, not just
    // missing (which allows the AuthProvider to attempt a refresh first).
    return body?.code === 'TOKEN_INVALID';
  } catch {
    return false;
  }
}

function isRefreshUrl(url: string): boolean {
  try {
    return new URL(url).pathname.endsWith('/auth/refresh');
  } catch {
    return false;
  }
}

export function ApiClientProvider({
  children,
  baseUrl,
}: ApiClientProviderProps) {
  const accessTokenRef = useRef<string | null>(null);

  const setAccessToken = useCallback((token: string | null) => {
    accessTokenRef.current = token;
  }, []);

  const clearAuth = useCallback(() => {
    accessTokenRef.current = null;
    if (typeof window !== 'undefined') {
      window.dispatchEvent(new CustomEvent(AUTH_SESSION_EXPIRED_EVENT));
    }
  }, []);

  // Resolve a path-relative `baseUrl` (e.g. "/api/v1" when proxied through a
  // dev server) against the current origin. The URL constructor rejects
  // relative bases, so without this the first plugin fetch throws
  // "Failed to construct 'URL': Invalid base URL".
  const resolvedBaseUrl = useMemo(() => {
    if (typeof window === 'undefined') return baseUrl;
    try {
      return new URL(baseUrl).toString();
    } catch {
      return new URL(baseUrl, window.location.origin).toString();
    }
  }, [baseUrl]);

  // An expired access token is a routine event in the dual-token scheme: the
  // JWT lives ~5 minutes while the HttpOnly refresh cookie lives for days, and
  // browsers throttle the background refresh interval in inactive/slept tabs.
  // So a 401 TOKEN_INVALID must first attempt a refresh; only when the refresh
  // itself is rejected is the session actually over. Concurrent 401s (e.g. a
  // refetch-on-focus burst after waking a laptop) and the AuthProvider's
  // background refresh all share one refresh (see refresh-session.ts).
  const refreshUrl = useMemo(
    () =>
      `${
        resolvedBaseUrl.endsWith('/')
          ? resolvedBaseUrl.slice(0, -1)
          : resolvedBaseUrl
      }/auth/refresh`,
    [resolvedBaseUrl],
  );

  const refreshSession = useCallback(
    () => refreshSharedSession(refreshUrl),
    [refreshUrl],
  );

  const refreshAccessToken = useCallback(async (): Promise<string | null> => {
    const session = await refreshSession();
    return session?.token ?? null;
  }, [refreshSession]);

  // Returns the new access token after a successful refresh (also storing it
  // for subsequent requests), or null after clearing the session because the
  // refresh token itself was rejected.
  const recoverSession = useCallback(async (): Promise<string | null> => {
    const token = await refreshAccessToken();
    if (!token) {
      clearAuth();
      return null;
    }
    accessTokenRef.current = token;
    return token;
  }, [refreshAccessToken, clearAuth]);

  const apiClient = useMemo(() => {
    const client = createClient<paths>({
      baseUrl: resolvedBaseUrl,
      credentials: 'include',
    });

    // Pristine clones of in-flight requests (keyed by middleware request id)
    // so they can be replayed after a token refresh; request bodies are
    // single-use streams, so the original cannot be re-fetched.
    const replayableRequests = new Map<string, Request>();

    client.use({
      onRequest({ id, request }) {
        const currentToken = accessTokenRef.current;
        if (currentToken) {
          request.headers.set('Authorization', `Bearer ${currentToken}`);
        }
        replayableRequests.set(id, request.clone());
        return request;
      },
      onError({ id }) {
        replayableRequests.delete(id);
      },
      async onResponse({ id, request, response }) {
        const replay = replayableRequests.get(id);
        replayableRequests.delete(id);

        if (!(await isTokenInvalidResponse(response))) {
          return response;
        }

        // A rejected /auth/refresh means the refresh token itself is dead;
        // attempting another refresh would loop.
        if (isRefreshUrl(request.url)) {
          clearAuth();
          return response;
        }

        const token = await recoverSession();
        if (!token) {
          return response;
        }

        const retryRequest = replay ?? (request.bodyUsed ? null : request);
        if (!retryRequest) {
          // Cannot replay this request, but the token has been refreshed so
          // the caller's own retry (e.g. TanStack Query) will succeed.
          return response;
        }

        retryRequest.headers.set('Authorization', `Bearer ${token}`);
        return fetch(retryRequest);
      },
    });

    return client;
  }, [resolvedBaseUrl, clearAuth, recoverSession]);

  const apiFetch = useMemo(
    () => async (input: string | URL, init?: RequestInit) => {
      const headers = new Headers(init?.headers);
      const hasCallerAuth = headers.has('Authorization');
      const currentToken = accessTokenRef.current;
      if (currentToken && !hasCallerAuth) {
        headers.set('Authorization', `Bearer ${currentToken}`);
      }

      const url = new URL(input, resolvedBaseUrl);
      const response = await fetch(url, {
        ...init,
        headers,
      });

      if (!(await isTokenInvalidResponse(response))) {
        return response;
      }

      if (isRefreshUrl(url.toString())) {
        clearAuth();
        return response;
      }

      const token = await recoverSession();
      if (!token) {
        return response;
      }

      // Only replay when we attached the token ourselves and the body is
      // reusable; a stream body was consumed by the first attempt.
      if (hasCallerAuth || init?.body instanceof ReadableStream) {
        return response;
      }

      const retryHeaders = new Headers(init?.headers);
      retryHeaders.set('Authorization', `Bearer ${token}`);
      return fetch(url, {
        ...init,
        headers: retryHeaders,
      });
    },
    [resolvedBaseUrl, clearAuth, recoverSession],
  );

  return (
    <ApiClientContext
      value={{ apiClient, apiFetch, setAccessToken, refreshSession }}
    >
      {children}
    </ApiClientContext>
  );
}
