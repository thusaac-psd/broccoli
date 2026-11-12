import type { RefreshedSession, RefreshOutcome } from '@/api/refresh-session';
import type { User } from '@/auth/types';

// The browser's sign-in session, owned by one store per API client.
//
// - `bootstrapping`: a page load with the session hint set, asking the
//   server whether the refresh cookie still holds. `retryInMs` is set while
//   the server is unreachable.
// - `signedOut`: no session. Only a login leaves it; a refresh never does.
// - `active`: signed in. `refreshInMs` is when the token is renewed next: a
//   full interval after a renewal, sooner (with backoff) while the server is
//   unreachable.
//
// `epoch` counts sign-ins and sign-outs. A refresh records the epoch it
// started in, and its outcome is dropped if the epoch has moved since: a
// refresh still in flight when the user signs out can never sign them back
// in, and one that started before a login cannot replace its session.

/** Renew the access token (which lives about five minutes) this often. */
export const SESSION_REFRESH_INTERVAL_MS = 4.5 * 60 * 1000;

export type SessionState =
  | { status: 'bootstrapping'; epoch: number; retryInMs: number | null }
  | { status: 'signedOut'; epoch: number }
  | {
      status: 'active';
      epoch: number;
      token: string;
      user: User;
      refreshInMs: number;
    };

export type SessionEvent =
  /** The bootstrap found no session hint: nobody signed in on this browser. */
  | { type: 'no-session' }
  | { type: 'signed-in'; session: RefreshedSession }
  /** Logout, a sign-out in another tab, or a definitive auth rejection. */
  | { type: 'signed-out' }
  | { type: 'refresh-settled'; epoch: number; outcome: RefreshOutcome };

export const INITIAL_SESSION: SessionState = {
  status: 'bootstrapping',
  epoch: 0,
  retryInMs: null,
};

function activate(epoch: number, session: RefreshedSession): SessionState {
  const { token, id, username, display_name, roles, permissions } = session;
  return {
    status: 'active',
    epoch,
    token,
    user: { id, username, display_name, roles, permissions },
    refreshInMs: SESSION_REFRESH_INTERVAL_MS,
  };
}

/** The next state; `state` itself when `event` changes nothing. */
export function transition(
  state: SessionState,
  event: SessionEvent,
): SessionState {
  switch (event.type) {
    case 'no-session':
      return state.status === 'bootstrapping'
        ? { status: 'signedOut', epoch: state.epoch }
        : state;
    case 'signed-in':
      return activate(state.epoch + 1, event.session);
    case 'signed-out':
      return state.status === 'signedOut'
        ? state
        : { status: 'signedOut', epoch: state.epoch + 1 };
    case 'refresh-settled': {
      if (event.epoch !== state.epoch || state.status === 'signedOut') {
        return state;
      }
      const { outcome } = event;
      switch (outcome.status) {
        case 'refreshed':
          return activate(state.epoch, outcome.session);
        case 'rejected':
          return { status: 'signedOut', epoch: state.epoch + 1 };
        case 'unavailable':
          return state.status === 'bootstrapping'
            ? { ...state, retryInMs: outcome.retryInMs }
            : { ...state, refreshInMs: outcome.retryInMs };
      }
    }
  }
}

/** When the next background refresh is due, or null when none is. */
export function nextRefreshInMs(state: SessionState): number | null {
  switch (state.status) {
    case 'bootstrapping':
      return state.retryInMs;
    case 'active':
      return state.refreshInMs;
    case 'signedOut':
      return null;
  }
}

/**
 * What a transition means outside the store. `hint` is the session hint to
 * store (shared with every tab through localStorage), `queries` what happens
 * to cached server data.
 */
export interface SessionEffects {
  hint: boolean | null;
  queries: 'clear' | 'invalidate' | null;
}

export function sessionEffects(
  prev: SessionState,
  next: SessionState,
): SessionEffects {
  if (next.epoch !== prev.epoch) {
    // Another account's data, or none: nothing cached may be shown again.
    return { hint: next.status === 'active', queries: 'clear' };
  }
  if (prev.status === 'bootstrapping' && next.status === 'active') {
    // Queries that ran before the token was restored got a 401.
    return { hint: true, queries: 'invalidate' };
  }
  return { hint: null, queries: null };
}

export type SessionListener = (next: SessionState, prev: SessionState) => void;

export interface Session {
  getState: () => SessionState;
  /** Listeners run synchronously on every state change. */
  subscribe: (listener: SessionListener) => () => void;
  dispatch: (event: SessionEvent) => void;
  /**
   * Exchange the refresh cookie for a new token. Resolves to that token when
   * it renewed the current session, else null (signed out meanwhile, the
   * server unreachable, or the session rejected). Never signs anyone in.
   */
  refresh: () => Promise<string | null>;
  /**
   * Sign out now, then run `revoke` (the logout request) once no refresh is
   * rotating the cookie, so it revokes the cookie the browser ends up with.
   */
  signOut: (revoke: () => Promise<unknown>) => Promise<void>;
}

export interface SessionDeps {
  /** The one shared refresh (see `refresh-session.ts`). */
  refresh: () => Promise<RefreshOutcome>;
  /** Runs `run` once no refresh is rotating the cookie. */
  withSettledCookie: <T>(run: () => Promise<T>) => Promise<T>;
}

export function createSession(deps: SessionDeps): Session {
  let state = INITIAL_SESSION;
  const listeners = new Set<SessionListener>();

  const dispatch = (event: SessionEvent) => {
    const prev = state;
    const next = transition(prev, event);
    if (next === prev) return;
    state = next;
    listeners.forEach((listener) => listener(next, prev));
  };

  return {
    getState: () => state,
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    dispatch,
    async refresh() {
      const { epoch, status } = state;
      if (status === 'signedOut') return null;
      const outcome = await deps.refresh();
      dispatch({ type: 'refresh-settled', epoch, outcome });
      return outcome.status === 'refreshed' &&
        state.status === 'active' &&
        state.epoch === epoch
        ? state.token
        : null;
    },
    async signOut(revoke) {
      dispatch({ type: 'signed-out' });
      await deps.withSettledCookie(revoke);
    },
  };
}

/** The token requests carry, if signed in. */
export function accessToken(state: SessionState): string | null {
  return state.status === 'active' ? state.token : null;
}
