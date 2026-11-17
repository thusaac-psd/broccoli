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
  | QueryFailure
  | { status: 'empty'; data: T; refreshError: unknown; retry: () => void }
  | { status: 'data'; data: T; refreshError: unknown; retry: () => void };

/**
 * A query that failed with nothing to show, as `useQueryState` reports it:
 * what `QueryErrorState` shows, so a view shows a failure only as the one
 * rule decides it.
 */
export interface QueryFailure {
  status: 'error';
  error: unknown;
  kind: ErrorKind;
  retry: () => void;
  retrying: boolean;
}

/** The fields of a TanStack Query result that decide its `QueryState`. */
export interface QueryLike<T> {
  data: T | undefined;
  error: unknown;
  /**
   * The error of the latest failed attempt while TanStack still retries,
   * before it reports `error`.
   */
  failureReason: unknown;
  isError: boolean;
  isFetching: boolean;
  fetchStatus?: 'fetching' | 'paused' | 'idle';
  /** How many fetches of this query have failed, in all. */
  errorUpdateCount?: number;
  refetch: () => unknown;
}

/**
 * The failure `query` shows while it has nothing to show: its error, or the
 * error of an attempt it is retrying. `lastError` is the error it last failed
 * with: a query asked again with nothing to show starts over as loading,
 * with no error, and the failure it had stays on screen until the new answer.
 */
function shownFailure(query: QueryLike<unknown>, lastError: unknown): unknown {
  if (query.isError) return query.error;
  if (query.failureReason != null) return query.failureReason;
  return query.isFetching && !!query.errorUpdateCount ? lastError : null;
}

/**
 * The state of `query`: once a first load has failed, its failure, whether
 * TanStack is still retrying it or a poll asks again behind it, and never a
 * load that seems to go on.
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
  const failure = shownFailure(query, lastError);
  if (failure != null) {
    return {
      status: 'error',
      error: failure,
      kind: errorKind(failure),
      retry,
      retrying: query.isFetching,
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
  const failure = query.isError ? query.error : query.failureReason;
  if (failure != null && failure !== lastError) setLastError(failure);
  return queryState(query, isEmpty, failure ?? lastError);
}
