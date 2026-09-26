import { useAuth } from '@broccoli/web-sdk/auth';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect } from 'react';

import { type Side, sideOf } from '../lib/stage';
import type { MatchView } from '../types';
import { useBracketApi } from './useBracketApi';

/**
 * The viewer's own current match (their latest round), from the same cached
 * `GET /bracket` the ranking page polls. Null for staff, spectators, and
 * players with no match yet.
 */
export function useMyMatch(
  contestId: number | undefined,
): { match: MatchView; side: Side } | null {
  const api = useBracketApi();
  const auth = useAuth();
  const viewerId = auth.user?.id ?? null;
  const { data } = useQuery({
    queryKey: ['codelink-bracket-bracket', contestId],
    enabled: !!contestId && viewerId !== null,
    queryFn: () => api.getBracket(contestId as number),
    refetchInterval: 5_000,
  });
  const match = (data?.matches ?? [])
    .filter((m) => sideOf(m, viewerId) !== null)
    .sort((a, b) => b.round - a.round)[0];
  const side = match ? sideOf(match, viewerId) : null;
  useRevealRefresh(contestId, match);
  return match && side ? { match, side } : null;
}

/**
 * Refetch the contest's problem lists (the host sidebar and problem table,
 * and this plugin's label lookup) whenever the viewer's match moves on: it
 * starts, a new game opens, or they reach a new match. Each of those reveals
 * a problem the lists fetched earlier could not include, and nothing else
 * would refresh them until a page reload.
 */
export function useRevealRefresh(
  contestId: number | undefined,
  match: MatchView | undefined,
) {
  const queryClient = useQueryClient();
  const stage = match
    ? `${match.id}:${match.state}:${match.current_xiaoju_index ?? '-'}`
    : null;
  useEffect(() => {
    if (!contestId || stage === null) return;
    void queryClient.invalidateQueries({
      queryKey: ['contest-problems', contestId],
    });
    void queryClient.invalidateQueries({
      queryKey: ['codelink-bracket-problems', contestId],
    });
  }, [contestId, stage, queryClient]);
}
