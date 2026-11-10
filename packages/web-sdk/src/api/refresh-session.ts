import type { components } from '@/api/schema';

export type RefreshedSession = components['schemas']['LoginResponse'];

/**
 * What a refresh found out about the session.
 *
 * - `refreshed`: the cookie was exchanged for a new access token.
 * - `rejected`: the server definitively refused the cookie; the session is
 *   over and the caller should sign the user out.
 * - `unavailable`: the server could not be asked (offline, a gateway 502, a
 *   database blip, rate limiting). The session may well be fine, so keep it
 *   and ask again in `retryInMs`.
 */
export type RefreshOutcome =
  | { status: 'refreshed'; session: RefreshedSession }
  | { status: 'rejected' }
  | { status: 'unavailable'; retryInMs: number };

// The refresh cookie is single-use: each successful refresh rotates it, and
// the server answers a second refresh that presents the old cookie with 401
// TOKEN_INVALID (a soft reject inside its reuse grace window). Every refresh
// in the app therefore goes through this one function so they never overlap:
// callers in one page share the in-flight request, and pages in other tabs,
// which share the cookie, wait for a cross-tab lock (a Web Lock, or a
// localStorage lease where Web Locks are unavailable).
const REFRESH_LOCK = 'broccoli:auth:refresh';

// A TOKEN_INVALID can still come from a refresh that raced with a tab running
// an older build without the lock, or with a tab that took the storage lease
// (below) in the same instant. By the time this delay has passed the
// winner's rotated cookie is in place, so one retry tells a lost race from a
// session that has really ended. A transient failure gets the same single
// quick retry, which rides out a momentary blip.
export const REFRESH_RETRY_DELAY_MS = 1_000;

// A refresh holds the cross-tab lock, so one stalled on a dead connection
// (a pulled LAN cable leaves TCP waiting for minutes) would stall every tab.
// Generous, so a slow but working server is not cut off mid-rotation.
const REFRESH_TIMEOUT_MS = 20_000;

// After the quick retry, transient failures back off exponentially up to
// this cap. The jitter keeps a room full of contestants from all retrying at
// the instant the server comes back.
const BACKOFF_BASE_MS = 2_000;
const BACKOFF_MAX_MS = 30_000;

// Only these, on a 401/403 from the refresh endpoint itself, mean the server
// looked at the cookie and refused it. Anything else (a gateway error page, a
// 5xx, a 429) says nothing about the session.
const AUTH_REJECTION_CODES = new Set([
  'TOKEN_MISSING',
  'TOKEN_INVALID',
  'PERMISSION_DENIED',
]);

export interface RefreshDeps {
  fetch: typeof fetch;
  sleep: (ms: number) => Promise<void>;
  withLock: <T>(run: () => Promise<T>) => Promise<T>;
  now: () => number;
  random: () => number;
}

const defaultDeps: RefreshDeps = {
  fetch: (input, init) => fetch(input, init),
  sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  withLock: async (run) =>
    typeof navigator !== 'undefined' && navigator.locks?.request
      ? await navigator.locks.request(REFRESH_LOCK, run)
      : withStorageLease(run),
  now: () => Date.now(),
  random: () => Math.random(),
};

// Web Locks exist only in secure contexts, and a LAN deployment over plain
// http has none. There, tabs take turns through a lease in localStorage: a tab
// that finds another tab's unexpired lease waits until it is released (the
// `storage` event, so no polling) or expires. Two tabs can still take it in
// the same instant; the TOKEN_INVALID retry covers that rare case.
const LEASE_KEY = 'broccoli:auth:refresh-lease';
// Long enough for a whole attempt: two timed-out requests and the pause.
const LEASE_MS = 2 * REFRESH_TIMEOUT_MS + REFRESH_RETRY_DELAY_MS;
const tabId = Math.random().toString(36).slice(2);

export async function withStorageLease<T>(run: () => Promise<T>): Promise<T> {
  let storage: Storage;
  try {
    storage = window.localStorage;
  } catch {
    return run();
  }
  const peerLeaseExpiry = () => {
    const [owner, expires] = (storage.getItem(LEASE_KEY) ?? '').split(':');
    return owner && owner !== tabId ? Number(expires) || 0 : 0;
  };

  for (
    let expires = peerLeaseExpiry();
    expires > Date.now();
    expires = peerLeaseExpiry()
  ) {
    await storageChange(LEASE_KEY, expires - Date.now());
  }

  try {
    storage.setItem(LEASE_KEY, `${tabId}:${Date.now() + LEASE_MS}`);
  } catch {
    // Storage full or blocked: refresh without the lease.
  }
  try {
    return await run();
  } finally {
    try {
      if (storage.getItem(LEASE_KEY)?.startsWith(`${tabId}:`)) {
        storage.removeItem(LEASE_KEY);
      }
    } catch {
      // The lease expires on its own.
    }
  }
}

/** Resolves when another tab changes `key`, or after `timeoutMs`. */
function storageChange(key: string, timeoutMs: number): Promise<void> {
  return new Promise((resolve) => {
    const done = () => {
      clearTimeout(timer);
      window.removeEventListener('storage', onStorage);
      resolve();
    };
    const onStorage = (event: StorageEvent) => {
      if (event.key === key || event.key === null) done();
    };
    const timer = setTimeout(done, timeoutMs);
    window.addEventListener('storage', onStorage);
  });
}

interface RefreshState {
  inFlight: Promise<RefreshOutcome> | null;
  /** Consecutive `unavailable` outcomes. */
  failures: number;
  /** No request is sent before this time; callers get `unavailable`. */
  retryAt: number;
}

const states = new Map<string, RefreshState>();

// What one response says; `lost-race` is a TOKEN_INVALID worth one retry.
type Verdict = RefreshedSession | 'rejected' | 'lost-race' | 'transient';
type Settled = Exclude<Verdict, 'lost-race'>;

/**
 * Exchange the refresh cookie for a new session. Concurrent calls for the
 * same URL share one request, and while the server is unreachable calls
 * inside the backoff window resolve at once without a request.
 */
export function refreshSession(
  refreshUrl: string,
  deps: Partial<RefreshDeps> = {},
): Promise<RefreshOutcome> {
  const d = { ...defaultDeps, ...deps };
  let state = states.get(refreshUrl);
  if (!state) {
    state = { inFlight: null, failures: 0, retryAt: 0 };
    states.set(refreshUrl, state);
  }
  if (state.inFlight) return state.inFlight;

  const waitMs = state.retryAt - d.now();
  if (waitMs > 0) {
    return Promise.resolve({ status: 'unavailable', retryInMs: waitMs });
  }

  const current = state;
  const request = d
    .withLock(() => attemptRefresh(refreshUrl, d))
    .catch((): Settled => 'transient')
    .then((verdict): RefreshOutcome => {
      if (verdict === 'transient') {
        current.failures += 1;
        const retryInMs = backoffMs(current.failures, d.random);
        current.retryAt = d.now() + retryInMs;
        return { status: 'unavailable', retryInMs };
      }
      current.failures = 0;
      current.retryAt = 0;
      return verdict === 'rejected'
        ? { status: 'rejected' }
        : { status: 'refreshed', session: verdict };
    })
    .finally(() => {
      current.inFlight = null;
    });
  current.inFlight = request;
  return request;
}

async function attemptRefresh(
  refreshUrl: string,
  deps: RefreshDeps,
): Promise<Settled> {
  for (let attempt = 0; ; attempt += 1) {
    const verdict = await classify(refreshUrl, deps);
    if (verdict !== 'lost-race' && verdict !== 'transient') return verdict;
    // A second TOKEN_INVALID is not a race any more.
    if (attempt > 0) return verdict === 'lost-race' ? 'rejected' : verdict;
    await deps.sleep(REFRESH_RETRY_DELAY_MS);
  }
}

async function classify(
  refreshUrl: string,
  deps: RefreshDeps,
): Promise<Verdict> {
  let response: Response;
  try {
    response = await deps.fetch(refreshUrl, {
      method: 'POST',
      credentials: 'include',
      signal:
        typeof AbortSignal.timeout === 'function'
          ? AbortSignal.timeout(REFRESH_TIMEOUT_MS)
          : undefined,
    });
  } catch {
    return 'transient';
  }

  const body = (await response.json().catch(() => null)) as {
    token?: unknown;
    code?: unknown;
  } | null;

  if (response.ok) {
    // A 2xx without a token is not our server talking (e.g. a captive
    // portal); it proves nothing about the session.
    return typeof body?.token === 'string'
      ? (body as RefreshedSession)
      : 'transient';
  }

  const code = typeof body?.code === 'string' ? body.code : null;
  if (
    (response.status === 401 || response.status === 403) &&
    code !== null &&
    AUTH_REJECTION_CODES.has(code)
  ) {
    return code === 'TOKEN_INVALID' ? 'lost-race' : 'rejected';
  }
  return 'transient';
}

function backoffMs(failures: number, random: () => number): number {
  const ceiling = Math.min(
    BACKOFF_MAX_MS,
    BACKOFF_BASE_MS * 2 ** Math.max(0, failures - 1),
  );
  // "Equal jitter": somewhere in the upper half of the window.
  return Math.round(ceiling / 2 + (random() * ceiling) / 2);
}
