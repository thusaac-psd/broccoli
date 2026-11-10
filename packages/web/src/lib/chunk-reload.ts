// A lazily loaded chunk can fail to load: the connection dropped for a moment,
// or the site was redeployed and the chunks this (cached) page points at are
// gone. Reloading fetches a fresh page and its current chunks, so that is the
// right answer, once. If the reload did not help, reloading again would only
// loop, so a second failure soon after shows an error page instead.

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

/** Whether `reloadForFreshChunks()` would reload (it has not just done so). */
export function canReloadForFreshChunks(): boolean {
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
