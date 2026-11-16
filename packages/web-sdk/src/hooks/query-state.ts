import { useState } from 'react';

import { type ErrorKind, errorKind } from '../api/errors.ts';

// What a view shows for a query. A failed fetch is never shown as "empty",
// "not found" or "no permission" unless that is what the server said, and a
// list with nothing in it is never shown before it has loaded.
//
// - `idle`: the query is not running (disabled) and has nothing to show.
// - `loading`: the first fetch is under way.
// - `error`: the fetch failed and there is nothing to show. `kind` says
//   what the user can do about it (sign in, retry, nothing). It stays while
//   the query asks again (`retrying`), by a poll or the user's retry.
// - `empty` / `data`: the data arrived. `refreshError` is set when a later
//   refetch failed; the last good data stays on screen.
export type QueryState<T> =
  | { status: 'idle' }
  | { status: 'loading' }
  | {
      status: 'error';
      error: unknown;
      kind: ErrorKind;
      retry: () => void;
      retrying: boolean;
    }
  | { status: 'empty'; data: T; refreshError: unknown; retry: () => void }
  | { status: 'data'; data: T; refreshError: unknown; retry: () => void };

/** The fields of a TanStack Query result that decide its `QueryState`. */
export interface QueryLike<T> {
  data: T | undefined;
  error: unknown;
  isError: boolean;
  isFetching: boolean;
  fetchStatus?: 'fetching' | 'paused' | 'idle';
  /** How many fetches of this query have failed, in all. */
  errorUpdateCount?: number;
  refetch: () => unknown;
}

/**
 * The state of `query`. `lastError` is the error it last failed with: a
 * query asked again with nothing to show starts over as loading, with no
 * error, and the failure it had stays on screen until the new answer.
 */
export function queryState<T>(
  query: QueryLike<T>,
  isEmpty?: (data: T) => boolean,
  lastError: unknown = null,
): QueryState<T> {
  const retry = () => void query.refetch();
  if (query.data !== undefined) {
    const refreshError = query.isError ? query.error : null;
    return isEmpty?.(query.data)
      ? { status: 'empty', data: query.data, refreshError, retry }
      : { status: 'data', data: query.data, refreshError, retry };
  }
  if (query.isError) {
    return {
      status: 'error',
      error: query.error,
      kind: errorKind(query.error),
      retry,
      retrying: query.isFetching,
    };
  }
  if (lastError != null && query.isFetching && !!query.errorUpdateCount) {
    return {
      status: 'error',
      error: lastError,
      kind: errorKind(lastError),
      retry,
      retrying: true,
    };
  }
  return query.fetchStatus === 'idle' && !query.isFetching
    ? { status: 'idle' }
    : { status: 'loading' };
}

/**
 * What a view shows for a query (see `QueryState`), keeping a failure on
 * screen while the query asks again: a poll of a page that never loaded
 * does not flicker between its error and loading.
 */
export function useQueryState<T>(
  query: QueryLike<T>,
  isEmpty?: (data: T) => boolean,
): QueryState<T> {
  const [lastError, setLastError] = useState<unknown>(null);
  if (query.isError && query.error !== lastError) setLastError(query.error);
  return queryState(query, isEmpty, query.isError ? query.error : lastError);
}
