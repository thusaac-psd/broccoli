import {
  backoffDelay,
  type BackoffPolicy,
  isKnownOffline,
  sleep,
} from '@broccoli/web-sdk/utils';

// The last resort for code that failed to load (see `chunk-load.ts` in the
// SDK, which retries every lazy import first): the site was most likely
// redeployed and the chunks this (cached) page points at are gone. Reloading
// fetches a fresh page and its current chunks, so that is the right answer,
// once. If the reload did not help, reloading again would only loop, so a
// second failure soon after shows an error page instead.
//
// A reload while the server cannot answer lands on the browser's own error
// page, which has no way back into the app. So nothing reloads until the
// server answers (`waitUntilReachable`): until then the in-app page says the
// connection is lost and keeps asking, with backoff, and reloads by itself
// once the server is back. The same goes while the browser knows it is
// offline.

const RELOAD_STAMP_KEY = 'broccoli:chunk-reload-at';
const RELOAD_GUARD_MS = 60_000;

function lastReloadAt(): number | null {
  try {
    const stamp = Number(sessionStorage.getItem(RELOAD_STAMP_KEY));
    return Number.isFinite(stamp) ? stamp : 0;
  } catch {
    // Without storage a reload loop could not be stopped: never reload.
    return null;
  }
}

/**
 * Whether `reloadForFreshChunks()` would reload: the browser is not known to
 * be offline, and it has not just done so.
 */
export function canReloadForFreshChunks(): boolean {
  if (isKnownOffline()) return false;
  const last = lastReloadAt();
  // The stamp outlives the page, so it takes the wall clock, not
  // `performance.now()`; `Math.abs` keeps a clock jump from blocking reloads.
  // eslint-disable-next-line no-restricted-properties
  return last !== null && Math.abs(Date.now() - last) >= RELOAD_GUARD_MS;
}

/**
 * Reload the page to pick up fresh chunks, unless that was just tried.
 * Returns whether it is reloading.
 */
export function reloadForFreshChunks(): boolean {
  if (!canReloadForFreshChunks()) return false;
  try {
    // eslint-disable-next-line no-restricted-properties -- see above
    sessionStorage.setItem(RELOAD_STAMP_KEY, String(Date.now()));
  } catch {
    return false;
  }
  window.location.reload();
  return true;
}

/**
 * The error page's retry: reload, unless the browser is known to be offline.
 * Returns whether it is reloading.
 */
export function retryPageLoad(): boolean {
  if (isKnownOffline()) return false;
  window.location.reload();
  return true;
}

export interface ReachabilityDeps {
  /** Whether the server answers right now. */
  probe: (signal: AbortSignal) => Promise<boolean>;
  sleep: (ms: number, signal: AbortSignal) => Promise<void>;
  random?: () => number;
}

const PROBE_BACKOFF: BackoffPolicy = { baseMs: 1_000, maxMs: 15_000 };

/** The page itself answers: what a reload would fetch first. */
async function probePage(signal: AbortSignal): Promise<boolean> {
  if (isKnownOffline()) return false;
  try {
    const response = await fetch(window.location.href, {
      method: 'HEAD',
      cache: 'no-store',
      signal,
    });
    return response.ok;
  } catch {
    return false;
  }
}

const defaultReachability: ReachabilityDeps = {
  probe: probePage,
  sleep,
  random: Math.random,
};

/**
 * Resolves true once the server answers (asking again with backoff until
 * it does), or false when `signal` aborts first.
 */
export async function waitUntilReachable(
  signal: AbortSignal,
  deps: ReachabilityDeps = defaultReachability,
): Promise<boolean> {
  for (let attempt = 0; !signal.aborted; attempt += 1) {
    if (await deps.probe(signal)) return !signal.aborted;
    await deps.sleep(backoffDelay(attempt, PROBE_BACKOFF, deps.random), signal);
  }
  return false;
}

/**
 * Where recovering from code that failed to load stands: waiting for the
 * server to answer, reloading, or failed (a reload was just tried and did not
 * help, so the page offers a manual retry instead of looping).
 */
export type ChunkRecovery = 'waiting' | 'reloading' | 'failed';
