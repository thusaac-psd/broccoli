import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { RefreshOutcome } from '../api/refresh-session.ts';
import {
  createSession,
  INITIAL_SESSION,
  nextRefreshInMs,
  SESSION_REFRESH_INTERVAL_MS,
  sessionEffects,
  type SessionState,
  transition,
} from './session.ts';

function login(token: string, id = 7) {
  return {
    token,
    id,
    username: `oj_${id}`,
    display_name: '选手',
    roles: ['contestant'],
    permissions: ['submission:submit'],
  };
}

const refreshed = (token: string): RefreshOutcome => ({
  status: 'refreshed',
  session: login(token),
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

function sessionWith(refresh: () => Promise<RefreshOutcome>) {
  const order: string[] = [];
  const session = createSession({
    refresh,
    withSettledCookie: async (run) => {
      order.push('revoke');
      return run();
    },
  });
  return { session, order };
}

test('a refresh that settles after sign-out does not sign the user back in', async () => {
  const pending = deferred<RefreshOutcome>();
  const { session } = sessionWith(() => pending.promise);
  session.dispatch({ type: 'signed-in', session: login('jwt-1') });

  const refreshing = session.refresh();
  await session.signOut(async () => undefined);
  pending.resolve(refreshed('jwt-2'));

  assert.equal(await refreshing, null);
  assert.equal(session.getState().status, 'signedOut');
});

test('a refresh that started before a login cannot replace its session', async () => {
  const pending = deferred<RefreshOutcome>();
  const { session } = sessionWith(() => pending.promise);
  const bootstrap = session.refresh();
  session.dispatch({ type: 'signed-in', session: login('jwt-login', 8) });
  pending.resolve(refreshed('jwt-stale'));

  assert.equal(await bootstrap, null);
  const state = session.getState();
  assert.equal(state.status === 'active' && state.token, 'jwt-login');
});

test('a refresh never signs anyone in from signed-out', async () => {
  let calls = 0;
  const { session } = sessionWith(async () => {
    calls += 1;
    return refreshed('jwt-2');
  });
  session.dispatch({ type: 'no-session' });

  assert.equal(await session.refresh(), null);
  assert.equal(calls, 0);
  assert.equal(session.getState().status, 'signedOut');
});

test('a refresh renews the token in place and returns it', async () => {
  const { session } = sessionWith(async () => refreshed('jwt-2'));
  session.dispatch({ type: 'signed-in', session: login('jwt-1') });

  assert.equal(await session.refresh(), 'jwt-2');
  const state = session.getState();
  assert.equal(state.status === 'active' && state.token, 'jwt-2');
});

test('an unreachable server keeps the session and its old token is not handed out as new', async () => {
  const { session } = sessionWith(async () => ({
    status: 'unavailable',
    retryInMs: 3_000,
  }));
  session.dispatch({ type: 'signed-in', session: login('jwt-1') });

  assert.equal(await session.refresh(), null);
  const state = session.getState();
  assert.equal(state.status, 'active');
  assert.equal(nextRefreshInMs(state), 3_000);
});

test('a rejected refresh signs out', async () => {
  const { session } = sessionWith(async () => ({ status: 'rejected' }));
  session.dispatch({ type: 'signed-in', session: login('jwt-1') });

  assert.equal(await session.refresh(), null);
  assert.equal(session.getState().status, 'signedOut');
});

test('logging in settles the bootstrap', () => {
  const next = transition(INITIAL_SESSION, {
    type: 'signed-in',
    session: login('jwt-1'),
  });
  assert.equal(next.status, 'active');
  assert.equal(nextRefreshInMs(next), SESSION_REFRESH_INTERVAL_MS);
});

test('the bootstrap keeps retrying while the server is unreachable', () => {
  const next = transition(INITIAL_SESSION, {
    type: 'refresh-settled',
    epoch: INITIAL_SESSION.epoch,
    outcome: { status: 'unavailable', retryInMs: 2_000 },
  });
  assert.equal(next.status, 'bootstrapping');
  assert.equal(nextRefreshInMs(next), 2_000);
});

test('sign-out revokes the cookie only after signing out locally', async () => {
  const { session, order } = sessionWith(async () => refreshed('jwt-2'));
  session.subscribe((next) => order.push(next.status));
  session.dispatch({ type: 'signed-in', session: login('jwt-1') });
  await session.signOut(async () => undefined);

  assert.deepEqual(order, ['active', 'signedOut', 'revoke']);
});

test('a sign-in or sign-out clears cached data and sets the hint for every tab', () => {
  const active = transition(INITIAL_SESSION, {
    type: 'signed-in',
    session: login('jwt-1'),
  });
  assert.deepEqual(sessionEffects(INITIAL_SESSION, active), {
    hint: true,
    queries: 'clear',
  });
  const out = transition(active, { type: 'signed-out' });
  assert.deepEqual(sessionEffects(active, out), {
    hint: false,
    queries: 'clear',
  });
});

test('a restored session refetches what ran without a token', () => {
  const active = transition(INITIAL_SESSION, {
    type: 'refresh-settled',
    epoch: INITIAL_SESSION.epoch,
    outcome: refreshed('jwt-1'),
  });
  assert.deepEqual(sessionEffects(INITIAL_SESSION, active), {
    hint: true,
    queries: 'invalidate',
  });
});

test('finding no session on load writes no hint', () => {
  const out: SessionState = transition(INITIAL_SESSION, { type: 'no-session' });
  assert.equal(out.status, 'signedOut');
  assert.deepEqual(sessionEffects(INITIAL_SESSION, out), {
    hint: null,
    queries: null,
  });
});

test('a background renewal has no outside effects', () => {
  const active = transition(INITIAL_SESSION, {
    type: 'signed-in',
    session: login('jwt-1'),
  });
  const renewed = transition(active, {
    type: 'refresh-settled',
    epoch: active.epoch,
    outcome: refreshed('jwt-2'),
  });
  assert.deepEqual(sessionEffects(active, renewed), {
    hint: null,
    queries: null,
  });
});
