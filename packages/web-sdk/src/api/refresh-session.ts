import type { components } from '@/api/schema';

export type RefreshedSession = components['schemas']['LoginResponse'];

// The refresh cookie is single-use: each successful refresh rotates it, and
// the server answers a second refresh that presents the old cookie with 401
// (a soft reject inside its reuse grace window). Every refresh in the app
// therefore goes through this one function so they never overlap: callers in
// one page share the in-flight request, and pages in other tabs, which share
// the cookie, wait for a cross-tab lock.
const REFRESH_LOCK = 'broccoli:auth:refresh';

// A 401 can still come from a refresh that raced with a tab running an older
// build without the lock. By the time this delay has passed the winner's
// rotated cookie is in place, so one retry tells a lost race from a session
// that has really ended.
export const REFRESH_RETRY_DELAY_MS = 1_000;

export interface RefreshDeps {
  fetch: typeof fetch;
  sleep: (ms: number) => Promise<void>;
  withLock: <T>(run: () => Promise<T>) => Promise<T>;
}

const defaultDeps: RefreshDeps = {
  fetch: (input, init) => fetch(input, init),
  sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  withLock: async (run) =>
    typeof navigator !== 'undefined' && navigator.locks?.request
      ? await navigator.locks.request(REFRESH_LOCK, run)
      : run(),
};

const inFlight = new Map<string, Promise<RefreshedSession | null>>();

/**
 * Exchange the refresh cookie for a new session, or resolve to null when the
 * session is over. Concurrent calls for the same URL share one request.
 */
export function refreshSession(
  refreshUrl: string,
  deps: RefreshDeps = defaultDeps,
): Promise<RefreshedSession | null> {
  const pending = inFlight.get(refreshUrl);
  if (pending) return pending;

  const request = deps
    .withLock(() => attemptRefresh(refreshUrl, deps))
    .finally(() => {
      inFlight.delete(refreshUrl);
    });
  inFlight.set(refreshUrl, request);
  return request;
}

async function attemptRefresh(
  refreshUrl: string,
  deps: RefreshDeps,
): Promise<RefreshedSession | null> {
  for (let attempt = 0; ; attempt += 1) {
    let response: Response | null = null;
    try {
      response = await deps.fetch(refreshUrl, {
        method: 'POST',
        credentials: 'include',
      });
    } catch {
      // Network failure: retried once like a lost race.
    }

    if (response?.ok) {
      try {
        const body = (await response.json()) as Partial<RefreshedSession>;
        return typeof body?.token === 'string'
          ? (body as RefreshedSession)
          : null;
      } catch {
        return null;
      }
    }

    const retryable = response === null || response.status === 401;
    if (!retryable || attempt > 0) return null;
    await deps.sleep(REFRESH_RETRY_DELAY_MS);
  }
}
