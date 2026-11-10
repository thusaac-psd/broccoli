import { useCallback, useState } from 'react';

import { createIdempotencyKeys, sendIdempotent } from '@/hooks/idempotency';

/**
 * Send POST requests with an Idempotency-Key bound to what they send.
 *
 * Pass the operation (everything that defines the request: path params and
 * body) and a function that sends it with the given key. Retrying the
 * identical operation, after a network error or a lost response, reuses its
 * key so the server can replay the original result instead of running it
 * twice. Changing anything about it (an edited file, another language,
 * another target) gets a fresh key, so the server never replays a stale
 * request in its place. Once the server accepts a request its key is retired,
 * so sending the same thing again on purpose is a new operation.
 *
 * Usage:
 * ```ts
 * const sendIdempotent = useIdempotentRequest();
 *
 * const { data, error } = await sendIdempotent(
 *   { contestId, body },
 *   (key) =>
 *     apiClient.POST('/contests/{id}/clarifications', {
 *       headers: { 'Idempotency-Key': key },
 *       params: { path: { id: contestId } },
 *       body,
 *     }),
 * );
 * ```
 */
export function useIdempotentRequest() {
  const [keys] = useState(createIdempotencyKeys);
  return useCallback(
    <T>(operation: unknown, request: (idempotencyKey: string) => Promise<T>) =>
      sendIdempotent(keys, operation, request),
    [keys],
  );
}
