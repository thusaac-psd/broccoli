import { errorKind, isTemporaryApiError } from '@broccoli/web-sdk/api';
import { QueryClient } from '@tanstack/react-query';

/** Retries of a query the server answered with a temporary error. */
const TEMPORARY_RETRIES = 6;

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

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30 * 1000,
      // A busy server (503) is waited out, and a lost connection or a
      // server error is retried once. A refusal (401, 403, 404, ...) is the
      // answer: retrying it only delays showing it.
      retry: (failures, error) => failures < retriesFor(error),
      // 2s, matching the server's Retry-After, doubling up to 16s.
      retryDelay: (attempt, error) =>
        isTemporaryApiError(error)
          ? Math.min(2_000 * 2 ** attempt, 16_000)
          : Math.min(1_000 * 2 ** attempt, 30_000),
    },
  },
});
