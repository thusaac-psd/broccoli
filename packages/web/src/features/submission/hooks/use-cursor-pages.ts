import { useCallback } from 'react';
import { useLocation, useSearchParams } from 'react-router';

/** History-state key holding the cursors of the pages before this one. */
const EARLIER_CURSORS = 'submissionListEarlierCursors';

function earlierCursors(state: unknown): string[] {
  if (typeof state !== 'object' || state === null) return [];
  const value = (state as Record<string, unknown>)[EARLIER_CURSORS];
  return Array.isArray(value) && value.every((c) => typeof c === 'string')
    ? value
    : [];
}

/**
 * Cursor paging for a submission list. The current page's cursor lives in
 * the `cursor` search param; the cursors of the pages before it live in the
 * history entry's state (`''` for the first page), so back/forward and
 * reload keep both. A deep link to a later page has no earlier cursors: its
 * page number is unknown and "previous" returns to the first page.
 */
export function useCursorPages() {
  const location = useLocation();
  const [searchParams, setSearchParams] = useSearchParams();
  const cursor = searchParams.get('cursor');
  const earlier = earlierCursors(location.state);

  const goTo = useCallback(
    (nextCursor: string | null, nextEarlier: string[]) => {
      const next = new URLSearchParams(searchParams);
      if (nextCursor) next.set('cursor', nextCursor);
      else next.delete('cursor');
      const state =
        typeof location.state === 'object' && location.state !== null
          ? location.state
          : {};
      setSearchParams(next, {
        state: { ...state, [EARLIER_CURSORS]: nextEarlier },
      });
    },
    [location.state, searchParams, setSearchParams],
  );

  const next = useCallback(
    (nextCursor: string) => goTo(nextCursor, [...earlier, cursor ?? '']),
    [cursor, earlier, goTo],
  );

  const previous = useCallback(
    () => goTo(earlier.at(-1) || null, earlier.slice(0, -1)),
    [earlier, goTo],
  );

  return {
    cursor,
    /** 1-based page number, or `null` when it is unknown. */
    page: cursor === null ? 1 : earlier.length > 0 ? earlier.length + 1 : null,
    hasPrevious: cursor !== null,
    next,
    previous,
  };
}
