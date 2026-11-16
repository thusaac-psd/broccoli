// Waiting between retries, shared by every retry loop in the app.

export interface BackoffPolicy {
  /** The first delay. */
  baseMs: number;
  /** No delay is longer than this. */
  maxMs: number;
}

/**
 * The delay before retry number `retry` (0 for the first retry): `baseMs`
 * doubling each time up to `maxMs`. With `random`, it is "equal jitter",
 * somewhere in the upper half of that, so many clients that failed together
 * do not all retry at the same instant.
 */
export function backoffDelay(
  retry: number,
  { baseMs, maxMs }: BackoffPolicy,
  random?: () => number,
): number {
  const ceiling = Math.min(maxMs, baseMs * 2 ** Math.max(0, retry));
  return random ? Math.round(ceiling / 2 + (random() * ceiling) / 2) : ceiling;
}

/** Resolves after `ms`, or as soon as `signal` aborts. */
export function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal?.aborted) return resolve();
    const done = () => {
      clearTimeout(timer);
      signal?.removeEventListener('abort', done);
      resolve();
    };
    const timer = setTimeout(done, ms);
    signal?.addEventListener('abort', done, { once: true });
  });
}
