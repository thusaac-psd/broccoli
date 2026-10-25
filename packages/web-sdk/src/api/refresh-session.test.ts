import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  REFRESH_RETRY_DELAY_MS,
  type RefreshDeps,
  refreshSession,
} from './refresh-session.ts';

const SESSION = {
  token: 'jwt-2',
  id: 7,
  username: 'oj_07',
  display_name: '选手',
  roles: ['contestant'],
  permissions: ['submission:submit'],
};

function respond(status: number, body: unknown = {}) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

let urlCounter = 0;
function freshUrl() {
  urlCounter += 1;
  return `http://oj.test/api/v1/auth/refresh?case=${urlCounter}`;
}

function deps(responses: Array<Response | Error>) {
  const calls: string[] = [];
  const sleeps: number[] = [];
  let locks = 0;
  const value: RefreshDeps = {
    fetch: async (input) => {
      calls.push(String(input));
      const next = responses.shift();
      if (!next) throw new Error('unexpected extra refresh');
      if (next instanceof Error) throw next;
      return next;
    },
    sleep: async (ms) => {
      sleeps.push(ms);
    },
    withLock: async (run) => {
      locks += 1;
      return run();
    },
  };
  return { value, calls, sleeps, locks: () => locks };
}

test('concurrent refreshes share one request', async () => {
  const url = freshUrl();
  const d = deps([respond(200, SESSION)]);
  const [a, b, c] = await Promise.all([
    refreshSession(url, d.value),
    refreshSession(url, d.value),
    refreshSession(url, d.value),
  ]);
  assert.equal(d.calls.length, 1);
  assert.equal(d.locks(), 1);
  assert.deepEqual(a, SESSION);
  assert.equal(a, b);
  assert.equal(b, c);
});

test('a refresh that lost a rotation race is retried once and succeeds', async () => {
  const d = deps([
    respond(401, { code: 'TOKEN_INVALID' }),
    respond(200, SESSION),
  ]);
  const session = await refreshSession(freshUrl(), d.value);
  assert.deepEqual(session, SESSION);
  assert.equal(d.calls.length, 2);
  assert.deepEqual(d.sleeps, [REFRESH_RETRY_DELAY_MS]);
});

test('two 401s mean the session is over', async () => {
  const d = deps([respond(401), respond(401)]);
  assert.equal(await refreshSession(freshUrl(), d.value), null);
  assert.equal(d.calls.length, 2);
});

test('a network error is retried once', async () => {
  const d = deps([new TypeError('Failed to fetch'), respond(200, SESSION)]);
  assert.deepEqual(await refreshSession(freshUrl(), d.value), SESSION);
});

test('other errors are not retried', async () => {
  const d = deps([respond(500)]);
  assert.equal(await refreshSession(freshUrl(), d.value), null);
  assert.equal(d.calls.length, 1);
  assert.deepEqual(d.sleeps, []);
});

test('a success without a token is not a session', async () => {
  const d = deps([respond(200, { id: 1 })]);
  assert.equal(await refreshSession(freshUrl(), d.value), null);
});

test('a later refresh sends a new request', async () => {
  const url = freshUrl();
  const first = deps([respond(200, SESSION)]);
  await refreshSession(url, first.value);
  const second = deps([respond(200, { ...SESSION, token: 'jwt-3' })]);
  const session = await refreshSession(url, second.value);
  assert.equal(second.calls.length, 1);
  assert.equal(session?.token, 'jwt-3');
});
