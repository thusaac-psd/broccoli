import { createContext, use, useMemo } from 'react';

import type { Session, SessionView } from '@/auth/session';

/**
 * The sign-in session every request takes its access token from, provided by
 * `ApiClientProvider`. Its `refresh` is the only way to refresh: concurrent
 * refreshes would rotate the single-use cookie out from under each other.
 * Only the auth and API providers hold it; everything else gets the
 * read-only `useSessionView`.
 */
export const SessionContext = createContext<Session | null>(null);

/** The session with its transitions, for the auth providers only. */
export function useSession(): Session {
  const session = use(SessionContext);
  if (!session) {
    throw new Error('useSession must be used within an ApiClientProvider');
  }
  return session;
}

/** The current session state and its changes, read-only. */
export function useSessionView(): SessionView {
  const { getState, subscribe } = useSession();
  return useMemo(() => ({ getState, subscribe }), [getState, subscribe]);
}
