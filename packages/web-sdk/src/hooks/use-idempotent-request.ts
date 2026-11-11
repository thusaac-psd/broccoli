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
 * A few refusals are not failures of the operation; tell them apart with
 * `idempotencyOutcome(error)`:
 * - `already-done`: an earlier send succeeded but its response is gone.
 *   Report success (without the data) and refetch.
 * - `outcome-unknown`: an earlier send was cut off and may have taken
 *   effect. Refetch the affected data and have the user check whether it
 *   happened before retrying; the key is retired, so a retry is a new
 *   operation.
 * - `in-progress`: an earlier send is still running. The key is kept, so
 *   retrying the identical operation in a moment gets its result.
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
