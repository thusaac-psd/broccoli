// A lazily loaded module can fail to load: the connection dropped for a
// moment, or the site was redeployed and the chunk a cached page points at is
// gone. Every importer recovers through this module: a failed load is retried
// with backoff, waiting for the connection while the browser knows it is
// offline, and only after that does the failure reach the caller (for a route,
// the root error boundary, which reloads the page once for fresh chunks).
//
// A browser remembers a failed `import()` of a URL for the life of the page
// and fails every later import of it without asking the network again, so a
// retry has to import a different URL: `retryImport` re-imports the failed
// module with a cache-busting query.

const CHUNK_LOAD_MESSAGE =
  /Failed to fetch dynamically imported module|error loading dynamically imported module|Importing a module script failed|is not a valid JavaScript MIME type|Unable to preload CSS|Loading (CSS )?chunk .+ failed/i;

/** Whether `error` is a failure to load code, not a bug in it. */
export function isChunkLoadError(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  return (
    error.name === 'ChunkLoadError' || CHUNK_LOAD_MESSAGE.test(error.message)
  );
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
  /** Which failures are worth another try. Chunk-load failures by default. */
  shouldRetry?: (error: unknown) => boolean;
  sleep?: (ms: number) => Promise<void>;
  whenOnline?: () => Promise<void>;
}

const RETRY_BASE_MS = 1_000;
const RETRY_MAX_MS = 15_000;
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
    shouldRetry = isChunkLoadError,
    sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
    whenOnline: online = whenOnline,
  }: RetryLoadOptions = {},
): Promise<T> {
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await load(attempt);
    } catch (error) {
      if (attempt + 1 >= attempts || !shouldRetry(error)) throw error;
      await sleep(Math.min(RETRY_MAX_MS, RETRY_BASE_MS * 2 ** attempt));
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
