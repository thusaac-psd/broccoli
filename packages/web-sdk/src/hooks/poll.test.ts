import assert from 'node:assert/strict';
import { test } from 'node:test';

import { RequestError } from '../api/errors.ts';
import { POLL_BACKOFF_MAX_MS, pollInterval } from './poll.ts';

interface Data {
  done: boolean;
}

const every = pollInterval<Data>((data) => (data?.done ? false : 1_000), {
  random: () => 1,
});

function polled(
  state: Partial<{
    data: Data;
    error: unknown;
    dataUpdateCount: number;
    errorUpdateCount: number;
  }>,
) {
  return {
    state: {
      data: undefined,
      error: null,
      dataUpdateCount: 0,
      errorUpdateCount: 0,
      ...state,
    },
  };
}

const serverError = RequestError.fromBody(502, {});

test('polls at its pace while polls succeed, and stops once the data says so', () => {
  assert.equal(every(polled({})), 1_000);
  assert.equal(
    every(polled({ data: { done: false }, dataUpdateCount: 1 })),
    1_000,
  );
  assert.equal(
    every(polled({ data: { done: true }, dataUpdateCount: 1 })),
    false,
  );
});

test('each failing poll in a row doubles the wait, and a success resets it', () => {
  const query = polled({ data: { done: false }, dataUpdateCount: 1 });
  const fail = () => {
    query.state.error = serverError;
    query.state.errorUpdateCount += 1;
    return every(query);
  };
  assert.equal(every(query), 1_000);
  assert.deepEqual([fail(), fail(), fail()], [2_000, 4_000, 8_000]);
  for (let i = 0; i < 10; i += 1) fail();
  assert.equal(fail(), POLL_BACKOFF_MAX_MS);

  query.state.error = null;
  query.state.dataUpdateCount += 1;
  assert.equal(every(query), 1_000);
  assert.equal(fail(), 2_000);
});

test('a slow poll is never made faster by failing', () => {
  const slow = pollInterval<Data>(60_000);
  assert.equal(
    slow(
      polled({
        data: { done: false },
        error: serverError,
        dataUpdateCount: 1,
        errorUpdateCount: 1,
      }),
    ),
    60_000,
  );
});

test('a failure with nothing on screen stops polling: the error shows with its retry', () => {
  assert.equal(
    every(polled({ error: serverError, errorUpdateCount: 1 })),
    false,
  );
});

test('a failure asking again cannot change stops polling', () => {
  for (const status of [403, 404]) {
    assert.equal(
      every(
        polled({
          data: { done: false },
          error: RequestError.fromBody(status, {}),
          dataUpdateCount: 1,
          errorUpdateCount: 1,
        }),
      ),
      false,
    );
  }
});
