import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { RefreshOutcome } from '../api/refresh-session.ts';
import {
  createSession,
  INITIAL_SESSION,
  nextRefreshInMs,
  sentAs,
  SESSION_REFRESH_INTERVAL_MS,
  sessionEffects,
  type SessionState,
  SIGNED_OUT_HINT,
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

test('a rejected refresh signs out, as an expired session', async () => {
  const { session } = sessionWith(async () => ({ status: 'rejected' }));
  session.dispatch({ type: 'signed-in', session: login('jwt-1') });

  assert.equal(await session.refresh(), null);
  assert.deepEqual(session.getState(), {
    status: 'signedOut',
    epoch: 2,
    expired: true,
  });
});

test('signing out, or nobody signing in, is not an expired session', () => {
  const active = transition(INITIAL_SESSION, {
    type: 'signed-in',
    session: login('jwt-1'),
  });
  const out = transition(active, { type: 'signed-out' });
  assert.equal(out.status === 'signedOut' && out.expired, false);
  const none = transition(INITIAL_SESSION, { type: 'no-session' });
  assert.equal(none.status === 'signedOut' && none.expired, false);
});

test('signing in again clears an expired session', () => {
  const expired = transition(
    transition(INITIAL_SESSION, { type: 'signed-in', session: login('a') }),
    {
      type: 'refresh-settled',
      epoch: 1,
      outcome: { status: 'rejected' },
    },
  );
  assert.equal(expired.status === 'signedOut' && expired.expired, true);
  assert.equal(
    transition(expired, { type: 'signed-in', session: login('b') }).status,
    'active',
  );
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
    hint: '7',
    queries: 'clear',
  });
  const out = transition(active, { type: 'signed-out' });
  assert.deepEqual(sessionEffects(active, out), {
    hint: SIGNED_OUT_HINT,
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
    hint: '7',
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

test('a request rejected after the account changed is not sent again as the new account', async () => {
  const { session } = sessionWith(async () => refreshed('jwt-bob-2'));
  session.dispatch({ type: 'signed-in', session: login('jwt-alice', 7) });
  const sent = sentAs(session.getState());
  await session.signOut(async () => undefined);
  session.dispatch({ type: 'signed-in', session: login('jwt-bob', 8) });

  assert.equal(await session.tokenForRetry(sent), null);
});

test('a request rejected after a renewal is sent again with the renewed token, without another refresh', async () => {
  let refreshes = 0;
  const { session } = sessionWith(async () => {
    refreshes += 1;
    return refreshed(`jwt-${refreshes + 1}`);
  });
  session.dispatch({ type: 'signed-in', session: login('jwt-1') });
  const sent = sentAs(session.getState());
  await session.refresh();

  assert.equal(await session.tokenForRetry(sent), 'jwt-2');
  assert.equal(refreshes, 1);
});

test('a request rejected with the current token refreshes it', async () => {
  const { session } = sessionWith(async () => refreshed('jwt-2'));
  session.dispatch({ type: 'signed-in', session: login('jwt-1') });

  assert.equal(
    await session.tokenForRetry(sentAs(session.getState())),
    'jwt-2',
  );
});

test('a renewal that comes back as another user switches accounts', () => {
  const alice = transition(INITIAL_SESSION, {
    type: 'signed-in',
    session: login('jwt-alice', 7),
  });
  const bob = transition(alice, {
    type: 'refresh-settled',
    epoch: alice.epoch,
    outcome: { status: 'refreshed', session: login('jwt-bob', 8) },
  });
  assert.notEqual(bob.epoch, alice.epoch);
  assert.deepEqual(sessionEffects(alice, bob), {
    hint: '8',
    queries: 'clear',
  });
});
