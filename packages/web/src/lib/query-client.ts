import { errorKind } from '@broccoli/web-sdk/api';
import { nextPollDelay } from '@broccoli/web-sdk/hooks';
import { QueryClient } from '@tanstack/react-query';

/** Retries of a query the server answered with a temporary error. */
const TEMPORARY_RETRIES = 6;

/** The first retry of a failed query waits about this long. */
const RETRY_BASE_MS = 1_000;

function retriesFor(error: unknown): number {
  switch (errorKind(error)) {
    case 'temporary':
      return TEMPORARY_RETRIES;
    case 'network':
    case 'server':
      return 1;
    default:
      return 0;
  }
}

/**
 * The wait before retry `attempt` (0 for the first) of a query that failed
 * with `error`: the shared poll backoff (`nextPollDelay`), jittered so the
 * seats a blip failed together do not retry in lockstep, and never sooner
 * than the server's `Retry-After`.
 */
export function queryRetryDelay(
  attempt: number,
  error: unknown,
  random: () => number = Math.random,
): number {
  return nextPollDelay(RETRY_BASE_MS, attempt + 1, { random, error });
}

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30 * 1000,
      // A busy server (503) is waited out, and a lost connection or a
      // server error is retried once. A refusal (401, 403, 404, ...) is the
      // answer: retrying it only delays showing it.
      retry: (failures, error) => failures < retriesFor(error),
      retryDelay: (attempt, error) => queryRetryDelay(attempt, error),
    },
  },
});
