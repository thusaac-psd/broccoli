import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  REFRESH_RETRY_DELAY_MS,
  type RefreshDeps,
  refreshSession,
  withStorageLease,
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

function respondText(status: number, text: string) {
  return new Response(text, {
    status,
    headers: { 'Content-Type': 'text/html' },
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
  let clock = 1_000_000;
  const value: Partial<RefreshDeps> = {
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
    now: () => clock,
    random: () => 0,
  };
  return {
    value,
    calls,
    sleeps,
    locks: () => locks,
    advance: (ms: number) => {
      clock += ms;
    },
    push: (...more: Array<Response | Error>) => responses.push(...more),
  };
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
  assert.deepEqual(a, { status: 'refreshed', session: SESSION });
  assert.equal(a, b);
  assert.equal(b, c);
});

test('a refresh that lost a rotation race is retried once and succeeds', async () => {
  const d = deps([
    respond(401, { code: 'TOKEN_INVALID' }),
    respond(200, SESSION),
  ]);
  const outcome = await refreshSession(freshUrl(), d.value);
  assert.deepEqual(outcome, { status: 'refreshed', session: SESSION });
  assert.equal(d.calls.length, 2);
  assert.deepEqual(d.sleeps, [REFRESH_RETRY_DELAY_MS]);
});

test('two TOKEN_INVALIDs mean the session is over', async () => {
  const d = deps([
    respond(401, { code: 'TOKEN_INVALID' }),
    respond(401, { code: 'TOKEN_INVALID' }),
  ]);
  assert.deepEqual(await refreshSession(freshUrl(), d.value), {
    status: 'rejected',
  });
  assert.equal(d.calls.length, 2);
});

test('a missing cookie or a deleted account is rejected at once', async () => {
  for (const [status, code] of [
    [401, 'TOKEN_MISSING'],
    [403, 'PERMISSION_DENIED'],
  ] as const) {
    const d = deps([respond(status, { code })]);
    assert.deepEqual(await refreshSession(freshUrl(), d.value), {
      status: 'rejected',
    });
    assert.equal(d.calls.length, 1);
    assert.deepEqual(d.sleeps, []);
  }
});

test('a network error is retried once', async () => {
  const d = deps([new TypeError('Failed to fetch'), respond(200, SESSION)]);
  assert.deepEqual(await refreshSession(freshUrl(), d.value), {
    status: 'refreshed',
    session: SESSION,
  });
});

test('transient failures keep the session', async () => {
  const transient = [
    () => new TypeError('Failed to fetch'),
    () => respond(500, { code: 'INTERNAL_ERROR' }),
    () => respondText(502, '<h1>Bad Gateway</h1>'),
    () => respond(503, { code: 'QUEUE_OVERLOADED' }),
    () => respond(408),
    () => respond(429, { code: 'RATE_LIMITED' }),
    // A 401 that is not the server refusing the cookie (e.g. a proxy page).
    () => respondText(401, 'Unauthorized'),
    // A 2xx that is not our server talking (e.g. a captive portal).
    () => respondText(200, '<html></html>'),
  ];
  for (const make of transient) {
    const d = deps([make(), make()]);
    const outcome = await refreshSession(freshUrl(), d.value);
    assert.equal(outcome.status, 'unavailable', String(make()));
    assert.equal(d.calls.length, 2);
  }
});

test('while unreachable, refreshes back off without sending requests', async () => {
  const url = freshUrl();
  const d = deps([respond(502), respond(502)]);

  const first = await refreshSession(url, d.value);
  assert.deepEqual(first, { status: 'unavailable', retryInMs: 1_000 });
  assert.equal(d.calls.length, 2);

  // Inside the window: no request, just the time left.
  d.advance(400);
  assert.deepEqual(await refreshSession(url, d.value), {
    status: 'unavailable',
    retryInMs: 600,
  });
  assert.equal(d.calls.length, 2);

  // Past it: asks again, and the window grows.
  d.advance(600);
  d.push(respond(502), respond(502));
  assert.deepEqual(await refreshSession(url, d.value), {
    status: 'unavailable',
    retryInMs: 2_000,
  });
  assert.equal(d.calls.length, 4);

  // Recovery resets the backoff.
  d.advance(2_000);
  d.push(respond(200, SESSION));
  assert.equal((await refreshSession(url, d.value)).status, 'refreshed');
  d.push(respond(502), respond(502));
  assert.deepEqual(await refreshSession(url, d.value), {
    status: 'unavailable',
    retryInMs: 1_000,
  });
});

test('the backoff is capped', async () => {
  const url = freshUrl();
  const d = deps([]);
  let last = 0;
  for (let i = 0; i < 10; i += 1) {
    d.push(respond(503), respond(503));
    const outcome = await refreshSession(url, d.value);
    assert.equal(outcome.status, 'unavailable');
    last = outcome.status === 'unavailable' ? outcome.retryInMs : 0;
    d.advance(last);
  }
  assert.equal(last, 15_000);
});

test('a failing lock is a transient failure', async () => {
  const d = deps([]);
  d.value.withLock = async () => {
    throw new DOMException('lock manager unavailable', 'SecurityError');
  };
  assert.equal(
    (await refreshSession(freshUrl(), d.value)).status,
    'unavailable',
  );
});

test('a later refresh sends a new request', async () => {
  const url = freshUrl();
  const first = deps([respond(200, SESSION)]);
  await refreshSession(url, first.value);
  const second = deps([respond(200, { ...SESSION, token: 'jwt-3' })]);
  const outcome = await refreshSession(url, second.value);
  assert.equal(second.calls.length, 1);
  assert.equal(
    outcome.status === 'refreshed' ? outcome.session.token : null,
    'jwt-3',
  );
});

test('without Web Locks, tabs take turns through a storage lease', async () => {
  const items = new Map<string, string>();
  const fakeWindow = Object.assign(new EventTarget(), {
    localStorage: {
      getItem: (key: string) => items.get(key) ?? null,
      setItem: (key: string, value: string) => void items.set(key, value),
      removeItem: (key: string) => void items.delete(key),
    },
  });
  const g = globalThis as { window?: unknown };
  const previous = g.window;
  g.window = fakeWindow;
  try {
    const key = 'broccoli:auth:refresh-lease';
    // Another tab is mid-refresh.
    items.set(key, `othertab:${Date.now() + 60_000}`);

    let ran = false;
    const turn = withStorageLease(async () => {
      ran = true;
      return items.get(key);
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    assert.equal(ran, false);

    // It finishes and releases the lease; this tab goes next.
    items.delete(key);
    fakeWindow.dispatchEvent(Object.assign(new Event('storage'), { key }));
    const heldDuringRun = await turn;
    assert.equal(ran, true);
    assert.ok(heldDuringRun && !heldDuringRun.startsWith('othertab:'));
    assert.equal(items.has(key), false);

    // An expired lease (its tab closed mid-refresh) does not block.
    items.set(key, `othertab:${Date.now() - 1}`);
    assert.equal(await withStorageLease(async () => 'ok'), 'ok');
  } finally {
    g.window = previous;
  }
});
