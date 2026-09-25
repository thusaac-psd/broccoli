import { useAuth } from '@broccoli/web-sdk/auth';
import { useQuery } from '@tanstack/react-query';

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
  return match && side ? { match, side } : null;
}
