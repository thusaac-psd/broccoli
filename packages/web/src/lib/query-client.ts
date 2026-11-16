import { isTemporaryApiError } from '@broccoli/web-sdk/api';
import { QueryClient } from '@tanstack/react-query';

/** Retries of a query the server answered with a temporary error. */
const TEMPORARY_RETRIES = 6;

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30 * 1000,
      // A busy server (503) is waited out; anything else is retried once.
      retry: (failures, error) =>
        failures < (isTemporaryApiError(error) ? TEMPORARY_RETRIES : 1),
      // 2s, matching the server's Retry-After, doubling up to 16s.
      retryDelay: (attempt, error) =>
        isTemporaryApiError(error)
          ? Math.min(2_000 * 2 ** attempt, 16_000)
          : Math.min(1_000 * 2 ** attempt, 30_000),
    },
  },
});
