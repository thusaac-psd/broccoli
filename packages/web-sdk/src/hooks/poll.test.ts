import assert from 'node:assert/strict';
import { test } from 'node:test';

import { RequestError } from '../api/errors.ts';
import { nextPollDelay, POLL_BACKOFF_MAX_MS, pollInterval } from './poll.ts';

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

test('a poll that has never loaded keeps asking, ever more slowly', () => {
  const query = polled({});
  const fail = () => {
    query.state.error = serverError;
    query.state.errorUpdateCount += 1;
    return every(query);
  };
  assert.deepEqual([fail(), fail(), fail()], [2_000, 4_000, 8_000]);
  for (let i = 0; i < 10; i += 1) fail();
  assert.equal(fail(), POLL_BACKOFF_MAX_MS);
});

test('a failure asking again cannot change stops polling, loaded or not', () => {
  for (const status of [403, 404]) {
    const error = RequestError.fromBody(status, {});
    assert.equal(
      every(
        polled({
          data: { done: false },
          error,
          dataUpdateCount: 1,
          errorUpdateCount: 1,
        }),
      ),
      false,
    );
    assert.equal(every(polled({ error, errorUpdateCount: 1 })), false);
  }
});

test('a failing poll waits a jittered backoff, never less than its pace', () => {
  assert.equal(
    nextPollDelay(1_000, 0, () => 0),
    1_000,
  );
  // Equal jitter: somewhere in the upper half of the doubled wait.
  assert.equal(
    nextPollDelay(1_000, 3, () => 0),
    4_000,
  );
  assert.equal(
    nextPollDelay(1_000, 3, () => 1),
    8_000,
  );
  assert.equal(
    nextPollDelay(1_000, 20, () => 1),
    POLL_BACKOFF_MAX_MS,
  );
  assert.equal(
    nextPollDelay(1_000, 20, () => 0),
    POLL_BACKOFF_MAX_MS / 2,
  );
  assert.equal(
    nextPollDelay(60_000, 5, () => 0),
    60_000,
  );
});

test('a failing poll keeps its jittered wait however often it is asked', () => {
  // TanStack asks for the interval on every render and restarts its timer
  // whenever the answer changes: a wait drawn anew each time would never end.
  let draws = 0;
  const jittered = pollInterval<Data>(1_000, {
    random: () => [0, 1][draws++ % 2],
  });
  const query = polled({
    data: { done: false },
    error: serverError,
    dataUpdateCount: 1,
    errorUpdateCount: 3,
  });
  const first = jittered(query);
  assert.equal(jittered(query), first);
  assert.equal(jittered(query), first);
  // The next failure draws its wait afresh.
  const drawn = draws;
  query.state.errorUpdateCount += 1;
  jittered(query);
  assert.equal(draws, drawn + 1);
});
