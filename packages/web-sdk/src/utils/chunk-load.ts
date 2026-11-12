// A lazily loaded module can fail to load: the connection dropped for a
// moment, or the site was redeployed and the chunk a cached page points at is
// gone. Lazy imports in app and SDK code recover through this module: a
// failed load is retried with backoff, waiting for the connection while the
// browser knows it is offline, and only after that does the failure reach the
// caller. Route modules are not imported through here: React Router reloads
// the page itself when one fails to load. Whatever still fails during render
// reaches the root error boundary, which reloads once for fresh chunks.
//
// A browser remembers a failed `import()` of a URL for the life of the page
// and fails every later import of it without asking the network again, so a
// retry has to import a different URL: `retryImport` re-imports the failed
// module with a cache-busting query.

import { backoffDelay, type BackoffPolicy, sleep } from './backoff.ts';

const CHUNK_LOAD_MESSAGE =
  /Failed to fetch dynamically imported module|error loading dynamically imported module|Importing a module script failed|is not a valid JavaScript MIME type|Unable to preload CSS|Loading (CSS )?chunk .+ failed/i;

/** Whether `error` is a failure to load code, not a bug in it. */
export function isChunkLoadError(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  return (
    error.name === 'ChunkLoadError' || CHUNK_LOAD_MESSAGE.test(error.message)
  );
}

const STYLESHEET_PRELOAD_MESSAGE = /Unable to preload CSS/i;

/**
 * Whether `error` is Vite failing to preload a chunk's stylesheet. Vite
 * marks a stylesheet as loaded before it is fetched and never asks for it
 * again, so a retry in the page would load the code without its styles: only
 * a page reload recovers (see the root's `vite:preloadError` handler).
 */
export function isStylesheetPreloadError(error: unknown): boolean {
  return (
    error instanceof Error && STYLESHEET_PRELOAD_MESSAGE.test(error.message)
  );
}

/** Whether a retry in the page can recover from `error`. */
export function isRetryableChunkLoadError(error: unknown): boolean {
  return isChunkLoadError(error) && !isStylesheetPreloadError(error);
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

/** Resolves once the browser is not known to be offline. */
export function whenOnline(): Promise<void> {
  if (!isKnownOffline()) return Promise.resolve();
  return new Promise((resolve) => {
    window.addEventListener('online', () => resolve(), { once: true });
  });
}

export interface RetryLoadOptions {
  /** Tries in all, the first included. */
  attempts?: number;
  /**
   * Which failures are worth another try. By default, chunk-load failures a
   * retry in the page can recover from.
   */
  shouldRetry?: (error: unknown) => boolean;
  sleep?: (ms: number) => Promise<void>;
  whenOnline?: () => Promise<void>;
}

const RETRY_BACKOFF: BackoffPolicy = { baseMs: 1_000, maxMs: 15_000 };
const DEFAULT_ATTEMPTS = 4;

/**
 * Run `load` (given the attempt number, so a retry can bust a cache) until it
 * succeeds or fails in a way not worth retrying, at most `attempts` times.
 * Retries back off exponentially and wait out a known offline spell, so
 * attempts are not spent while the request could not possibly succeed.
 */
export async function retryLoad<T>(
  load: (attempt: number) => Promise<T>,
  {
    attempts = DEFAULT_ATTEMPTS,
    shouldRetry = isRetryableChunkLoadError,
    sleep: wait = (ms) => sleep(ms),
    whenOnline: online = whenOnline,
  }: RetryLoadOptions = {},
): Promise<T> {
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await load(attempt);
    } catch (error) {
      if (attempt + 1 >= attempts || !shouldRetry(error)) throw error;
      await wait(backoffDelay(attempt, RETRY_BACKOFF));
      await online();
    }
  }
}

/**
 * The URL of the module a chunk-load error names (Chromium and Firefox put it
 * at the end of the message), or null.
 */
export function failedModuleUrl(error: unknown): string | null {
  if (!isChunkLoadError(error)) return null;
  const match = /(https?:\/\/\S+?)\.?$/.exec((error as Error).message);
  return match ? match[1] : null;
}

/** `url` with a query that makes the browser fetch it as a new module. */
export function retryUrl(url: string, attempt: number): string {
  const next = new URL(url);
  next.searchParams.set('retry', String(attempt));
  return next.toString();
}

/**
 * `retryLoad` for a lazy `import()`: each retry imports the module the last
 * failure named under a fresh URL, since the browser would fail the original
 * URL again without fetching it. Where the error names no URL, `load` itself
 * runs again.
 */
export function retryImport<T>(
  load: () => Promise<T>,
  options?: RetryLoadOptions,
): Promise<T> {
  let failedUrl: string | null = null;
  return retryLoad(async (attempt) => {
    try {
      return failedUrl
        ? ((await import(/* @vite-ignore */ retryUrl(failedUrl, attempt))) as T)
        : await load();
    } catch (error) {
      failedUrl = failedModuleUrl(error) ?? failedUrl;
      throw error;
    }
  }, options);
}
