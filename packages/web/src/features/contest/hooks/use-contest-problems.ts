import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuthReady } from '@broccoli/web-sdk/auth';
import { useQuery } from '@tanstack/react-query';
import { useEffect, useRef, useState } from 'react';

import { fetchContestProblemList } from '@/features/contest/api/fetch-contest-problem-list';
import { useContestInfo } from '@/features/contest/hooks/use-contest-info';
import { problemListRefetchInterval } from '@/features/contest/utils/problem-refresh';

/**
 * The problems of `contestId` the viewer may see. Every view of the list
 * shares this query, so all of them refresh together: periodically while
 * the contest runs (see `problemListRefetchInterval`) and once when it
 * starts.
 */
export function useContestProblems(
  contestId: number | null | undefined,
  { enabled = true }: { enabled?: boolean } = {},
) {
  const apiClient = useApiClient();
  const authReady = useAuthReady();
  const id = contestId ?? NaN;
  const { contest } = useContestInfo(id);
  const startMs = contest ? Date.parse(contest.start_time) : null;
  const started = usePassed(startMs);

  const query = useQuery({
    queryKey: ['contest-problems', contestId],
    enabled: authReady && Number.isFinite(id) && enabled,
    queryFn: () => fetchContestProblemList(apiClient, id),
    refetchInterval: () => problemListRefetchInterval(contest, Date.now()),
  });

  // Fetch again the moment the contest opens, when the list typically goes
  // from empty to full; not on first load of an already-running contest.
  const awaitingStart = useRef(false);
  const { refetch } = query;
  useEffect(() => {
    if (startMs === null) return;
    if (!started) {
      awaitingStart.current = true;
    } else if (awaitingStart.current) {
      awaitingStart.current = false;
      void refetch();
    }
  }, [startMs, started, refetch]);

  return query;
}

/**
 * Whether the wall clock has reached `atMs`, re-rendering once when it
 * does. `false` while `atMs` is unknown.
 */
export function usePassed(atMs: number | null): boolean {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (atMs === null || now >= atMs) return;
    // setTimeout fires at once for delays past 2^31-1 ms (~24.8 days).
    const delay = Math.min(atMs - now + 50, 2 ** 31 - 1);
    const timer = setTimeout(() => setNow(Date.now()), delay);
    return () => clearTimeout(timer);
  }, [atMs, now]);
  return atMs !== null && now >= atMs;
}
