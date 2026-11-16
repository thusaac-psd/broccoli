import assert from 'node:assert/strict';
import { test } from 'node:test';

import { RequestError } from '../api/errors.ts';
import { type QueryLike, queryState } from './query-state.ts';

function query<T>(fields: Partial<QueryLike<T>>): QueryLike<T> {
  return {
    data: undefined,
    error: null,
    isError: false,
    isFetching: false,
    fetchStatus: 'idle',
    refetch: () => undefined,
    ...fields,
  };
}

const isEmpty = (items: number[]) => items.length === 0;

test('a failed fetch is an error with its kind, never an empty list', () => {
  const state = queryState(
    query<number[]>({
      isError: true,
      error: new RequestError(500, 'INTERNAL_ERROR'),
    }),
    isEmpty,
  );
  assert.equal(state.status, 'error');
  assert.equal(state.status === 'error' && state.kind, 'server');
});

test('a list is empty only once it has arrived empty', () => {
  assert.equal(
    queryState(
      query<number[]>({ fetchStatus: 'fetching', isFetching: true }),
      isEmpty,
    ).status,
    'loading',
  );
  assert.equal(queryState(query({ data: [] }), isEmpty).status, 'empty');
  assert.equal(queryState(query({ data: [1] }), isEmpty).status, 'data');
});

test('a failed refetch keeps the last data and reports the failure', () => {
  const error = RequestError.network();
  const state = queryState(query({ data: [1], isError: true, error }), isEmpty);
  assert.equal(state.status, 'data');
  assert.equal(state.status === 'data' && state.refreshError, error);
});

test('a query that is not running is idle, not loading forever', () => {
  assert.equal(queryState(query<number[]>({})).status, 'idle');
});

test('retry refetches', () => {
  let refetched = 0;
  const state = queryState(
    query<number[]>({
      isError: true,
      error: new RequestError(503, 'PLUGIN_UNAVAILABLE'),
      refetch: () => (refetched += 1),
    }),
  );
  assert.equal(state.status === 'error' && state.kind, 'temporary');
  if (state.status === 'error') state.retry();
  assert.equal(refetched, 1);
});
