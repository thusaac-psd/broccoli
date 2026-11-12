import type {
  CodeRun,
  Submission,
  SubmissionError,
} from '@broccoli/web-sdk/submission';
import { backoffDelay,type BackoffPolicy } from '@broccoli/web-sdk/utils';

// The submissions and code runs started from one problem page, each moving
// through `submitting -> polling -> done`, or into `error` from either of the
// first two. `done` and `error` are final: no late response, retry or poll
// can move an entry out of them. Actions for entries that no longer exist
// (the problem changed while a submit was in flight) change nothing.

export type EntryStatus = 'submitting' | 'polling' | 'done' | 'error';

export interface SubmissionEntry {
  /** Unique client-side ID for this entry */
  id: number;
  submission: Submission | null;
  codeRun: CodeRun | null;
  status: EntryStatus;
  error: SubmissionError | null;
  /** True for "run code" entries */
  isRun?: boolean;
  /**
   * Shared key linking sibling entries created together by an admin fan-out
   * (one submit, N pinned submissions). Entries with the same groupKey are
   * rendered together as a comparison strip.
   */
  groupKey?: string;
  /**
   * Worker this entry was pinned to, if any. Used by the comparison strip and
   * the per-row label.
   */
  targetWorkerId?: string;
}

/** What the server reported for an entry, and whether it is final. */
export type EntryResult = (
  | { submission: Submission; codeRun?: never }
  | { codeRun: CodeRun; submission?: never }
) & { terminal: boolean };

export type EntryAction =
  | { type: 'added'; entries: SubmissionEntry[] }
  /** The server accepted the submit or run. */
  | { type: 'accepted'; id: number; result: EntryResult }
  | { type: 'polled'; id: number; result: EntryResult }
  | { type: 'failed'; id: number; error: SubmissionError }
  /** The entry has nothing to show here (it is in the submissions list). */
  | { type: 'withdrawn'; id: number }
  | { type: 'reset' };

/** The statuses each action may move an entry out of. */
const FROM: Record<
  Exclude<EntryAction['type'], 'added' | 'withdrawn' | 'reset'>,
  ReadonlySet<EntryStatus>
> = {
  accepted: new Set(['submitting']),
  polled: new Set(['polling']),
  failed: new Set(['submitting', 'polling']),
};

function withResult(
  entry: SubmissionEntry,
  { terminal, ...result }: EntryResult,
): SubmissionEntry {
  return {
    ...entry,
    ...(result.codeRun
      ? { codeRun: result.codeRun }
      : { submission: result.submission }),
    status: terminal ? 'done' : 'polling',
  };
}

export function entriesReducer(
  entries: SubmissionEntry[],
  action: EntryAction,
): SubmissionEntry[] {
  switch (action.type) {
    case 'added':
      return [...action.entries, ...entries];
    case 'withdrawn':
      return entries.some((e) => e.id === action.id)
        ? entries.filter((e) => e.id !== action.id)
        : entries;
    case 'reset':
      return entries.length === 0 ? entries : [];
    default: {
      const index = entries.findIndex((e) => e.id === action.id);
      const entry = entries[index];
      if (!entry || !FROM[action.type].has(entry.status)) return entries;
      const next =
        action.type === 'failed'
          ? { ...entry, status: 'error' as const, error: action.error }
          : withResult(entry, action.result);
      return entries.map((e, i) => (i === index ? next : e));
    }
  }
}

/** What an entry in `polling` polls: its submission or its code run. */
export function pollTarget(
  entry: SubmissionEntry,
): { kind: 'submission' | 'codeRun'; id: number } | null {
  if (entry.status !== 'polling') return null;
  if (entry.isRun) {
    return entry.codeRun ? { kind: 'codeRun', id: entry.codeRun.id } : null;
  }
  return entry.submission
    ? { kind: 'submission', id: entry.submission.id }
    : null;
}

export const POLL_INTERVAL_MS = 1_000;
const POLL_BACKOFF: BackoffPolicy = { baseMs: POLL_INTERVAL_MS, maxMs: 15_000 };

/** One poll: the result, or the HTTP status and body of a failed request. */
export type PollResponse =
  | { ok: true; result: EntryResult }
  | { ok: false; status: number | null; error: unknown };

export interface PollDeps {
  fetch: (signal: AbortSignal) => Promise<PollResponse>;
  sleep: (ms: number, signal: AbortSignal) => Promise<void>;
  dispatch: (action: EntryAction) => void;
  toError: (error: unknown) => SubmissionError;
}

// Asking again cannot change these: the entry is gone or hidden from this
// user, or the session is over.
const PERMANENT_STATUSES = new Set([401, 403, 404]);

/**
 * Poll one entry until it is final, one request at a time: the next poll is
 * scheduled only once the previous one has answered, so responses can never
 * arrive out of order. Transient failures back off; a permanent one fails the
 * entry. Stops when `signal` aborts.
 */
export async function pollEntry(
  entryId: number,
  signal: AbortSignal,
  deps: PollDeps,
): Promise<void> {
  // Consecutive transient failures.
  let failures = 0;
  while (!signal.aborted) {
    await deps.sleep(
      failures === 0 ? POLL_INTERVAL_MS : backoffDelay(failures, POLL_BACKOFF),
      signal,
    );
    if (signal.aborted) return;
    let response: PollResponse;
    try {
      response = await deps.fetch(signal);
    } catch (error) {
      response = { ok: false, status: null, error };
    }
    if (signal.aborted) return;
    if (response.ok) {
      deps.dispatch({ type: 'polled', id: entryId, result: response.result });
      if (response.result.terminal) return;
      failures = 0;
    } else if (
      response.status !== null &&
      PERMANENT_STATUSES.has(response.status)
    ) {
      deps.dispatch({
        type: 'failed',
        id: entryId,
        error: deps.toError(response.error),
      });
      return;
    } else {
      failures += 1;
    }
  }
}
