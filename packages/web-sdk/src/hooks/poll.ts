import { errorKind } from '../api/errors.ts';
import { backoffDelay } from '../utils/backoff.ts';

/** What `pollInterval` reads of a TanStack query (the `Query` itself). */
export interface PolledQuery<T> {
  state: {
    data: T | undefined;
    error: unknown;
    /** How many fetches succeeded, and how many failed, in all. */
    dataUpdateCount: number;
    errorUpdateCount: number;
  };
}

/** The longest a failing poll waits for the next one (unless it polls slower). */
export const POLL_BACKOFF_MAX_MS = 30_000;

export interface PollOptions {
  /** Spreads the waits of seats that failed together (default `Math.random`). */
  random?: () => number;
}

/** Per query: its success count, and its failure count when it last succeeded. */
const lastSuccess = new WeakMap<
  object,
  { dataUpdateCount: number; errorUpdateCount: number }
>();

/** How many polls in a row have failed. */
function failuresInARow(query: PolledQuery<unknown>): number {
  const { error, dataUpdateCount, errorUpdateCount } = query.state;
  let seen = lastSuccess.get(query);
  if (seen?.dataUpdateCount !== dataUpdateCount) {
    seen = {
      dataUpdateCount,
      errorUpdateCount: errorUpdateCount - (error == null ? 0 : 1),
    };
    lastSuccess.set(query, seen);
  }
  return errorUpdateCount - seen.errorUpdateCount;
}

/**
 * The `refetchInterval` of every query that polls. `every` is the pace, or
 * the pace for the data at hand (`false` stops: a final verdict, an ended
 * contest).
 *
 * - While polls fail, the last good data stays on screen (show the
 *   `refreshError` of `useQueryState`) and each failure doubles the wait, up to
 *   `POLL_BACKOFF_MAX_MS` and spread by jitter: a server in trouble is not
 *   asked once a second by every seat at once.
 * - A poll that has never loaded backs off the same way: a page opened
 *   while the server was down fills in once it is back. `useQueryState`
 *   keeps the error on screen while each poll asks again.
 * - A failure asking again cannot change (no permission, gone) stops.
 */
export function pollInterval<T>(
  every: number | ((data: T | undefined) => number | false),
  { random = Math.random }: PollOptions = {},
): (query: object) => number | false {
  // Takes any object, so TanStack infers none of the query's types from it;
  // what TanStack passes is a `Query`, which has what `PolledQuery` reads.
  return (polled) => {
    const query = polled as PolledQuery<T>;
    const { state } = query;
    const pace = typeof every === 'number' ? every : every(state.data);
    const failures = failuresInARow(query);
    if (pace === false || state.error == null) return pace;
    const kind = errorKind(state.error);
    if (kind === 'forbidden' || kind === 'notFound') return false;
    const policy = { baseMs: pace, maxMs: Math.max(pace, POLL_BACKOFF_MAX_MS) };
    return Math.max(pace, backoffDelay(failures, policy, random));
  };
}
