// A lazily loaded chunk can fail to load: the connection dropped for a moment,
// or the site was redeployed and the chunks this (cached) page points at are
// gone. Reloading fetches a fresh page and its current chunks, so that is the
// right answer, once. If the reload did not help, reloading again would only
// loop, so a second failure soon after shows an error page instead.
//
// While the browser knows it is offline a reload can only land on the
// browser's own offline page, which has no way back into the app, so none is
// attempted: the in-app error page stays up and retries once the connection
// is back.

const RELOAD_STAMP_KEY = 'broccoli:chunk-reload-at';
const RELOAD_GUARD_MS = 60_000;

const CHUNK_LOAD_MESSAGE =
  /Failed to fetch dynamically imported module|error loading dynamically imported module|Importing a module script failed|is not a valid JavaScript MIME type|Unable to preload CSS|Loading (CSS )?chunk .+ failed/i;

/** Whether `error` is a failure to load code, not a bug in it. */
export function isChunkLoadError(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  return (
    error.name === 'ChunkLoadError' || CHUNK_LOAD_MESSAGE.test(error.message)
  );
}

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
 * Whether the browser knows it has no network. Only `false` from
 * `navigator.onLine` is trustworthy; `true` just means some interface is up.
 */
export function isKnownOffline(): boolean {
  return typeof navigator !== 'undefined' && navigator.onLine === false;
}

/** Calls `listener` whenever the browser goes online or offline. */
export function subscribeToOnlineStatus(listener: () => void): () => void {
  window.addEventListener('online', listener);
  window.addEventListener('offline', listener);
  return () => {
    window.removeEventListener('online', listener);
    window.removeEventListener('offline', listener);
  };
}

/**
 * Whether `reloadForFreshChunks()` would reload: the browser is not known to
 * be offline, and it has not just done so.
 */
export function canReloadForFreshChunks(): boolean {
  if (isKnownOffline()) return false;
  const last = lastReloadAt();
  return last !== null && Math.abs(Date.now() - last) >= RELOAD_GUARD_MS;
}

/**
 * Reload the page to pick up fresh chunks, unless that was just tried.
 * Returns whether it is reloading.
 */
export function reloadForFreshChunks(): boolean {
  if (!canReloadForFreshChunks()) return false;
  try {
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
