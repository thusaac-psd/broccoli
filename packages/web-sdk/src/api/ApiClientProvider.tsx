import createClient from 'openapi-fetch';
import { type ReactNode, useMemo } from 'react';

import { ApiClientContext } from '@/api/api-client-context';
import { refreshSession, withSettledCookie } from '@/api/refresh-session';
import { withRequestErrors } from '@/api/request-errors';
import type { paths } from '@/api/schema';
import { fetchWithServerClock } from '@/api/server-clock';
import { createSession, type SentAs, sentAs } from '@/auth/session';
import { SessionContext } from '@/auth/session-context';

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

  // The one owner of the sign-in session: the AuthProvider drives its
  // lifecycle, and every request reads the access token from it.
  const session = useMemo(
    () =>
      createSession({
        refresh: () =>
          refreshSession(refreshUrl, { fetch: fetchWithServerClock }),
        withSettledCookie: (run) => withSettledCookie(refreshUrl, run),
      }),
    [refreshUrl],
  );

  const apiClient = useMemo(() => {
    const client = createClient<paths>({
      baseUrl: resolvedBaseUrl,
      credentials: 'include',
      fetch: fetchWithServerClock,
    });

    // In-flight requests (keyed by middleware request id): the session each
    // was sent in, and a pristine clone so it can be replayed with a renewed
    // token (request bodies are single-use streams, so the original cannot
    // be re-fetched).
    const inFlight = new Map<string, { sent: SentAs; replay: Request }>();

    client.use({
      async onRequest({ id, request }) {
        // The bootstrap's own refresh goes around this client, so waiting
        // here never holds it up.
        const sent = sentAs(await session.settled());
        if (sent.token) {
          request.headers.set('Authorization', `Bearer ${sent.token}`);
        }
        inFlight.set(id, { sent, replay: request.clone() });
        return request;
      },
      onError({ id }) {
        inFlight.delete(id);
      },
      async onResponse({ id, request, response }) {
        const pending = inFlight.get(id);
        inFlight.delete(id);

        if (!(await isTokenInvalidResponse(response))) {
          return response;
        }

        // Never answer a refresh's 401 with another refresh (it would loop).
        // Nor is it proof the session is over: a refresh sent around
        // `refreshSession` can lose a rotation race, so only that function
        // decides when a session has ended.
        if (isRefreshUrl(request.url)) return response;

        // Never replayed into another session than the one it was sent in.
        const token = pending && (await session.tokenForRetry(pending.sent));
        if (!token) {
          return response;
        }

        const retryRequest =
          pending?.replay ?? (request.bodyUsed ? null : request);
        if (!retryRequest) {
          // Cannot replay this request, but the token has been refreshed so
          // the caller's own retry (e.g. TanStack Query) will succeed.
          return response;
        }

        retryRequest.headers.set('Authorization', `Bearer ${token}`);
        return fetchWithServerClock(retryRequest);
      },
    });

    return withRequestErrors(client);
  }, [resolvedBaseUrl, session]);

  const apiFetch = useMemo(
    () => async (input: string | URL, init?: RequestInit) => {
      const headers = new Headers(init?.headers);
      const hasCallerAuth = headers.has('Authorization');
      const sent = sentAs(await session.settled());
      if (sent.token && !hasCallerAuth) {
        headers.set('Authorization', `Bearer ${sent.token}`);
      }

      const url = new URL(input, resolvedBaseUrl);
      const response = await fetchWithServerClock(url, {
        ...init,
        headers,
      });

      if (!(await isTokenInvalidResponse(response))) {
        return response;
      }

      if (isRefreshUrl(url.toString())) return response;

      const token = await session.tokenForRetry(sent);
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
      return fetchWithServerClock(url, {
        ...init,
        headers: retryHeaders,
      });
    },
    [resolvedBaseUrl, session],
  );

  const value = useMemo(
    () => ({ apiClient, baseUrl: resolvedBaseUrl, apiFetch }),
    [apiClient, resolvedBaseUrl, apiFetch],
  );

  return (
    <SessionContext value={session}>
      <ApiClientContext value={value}>{children}</ApiClientContext>
    </SessionContext>
  );
}
