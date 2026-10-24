import { useAuth } from '@broccoli/web-sdk/auth';
import { SUBMISSION_VIEW_ALL } from '@broccoli/web-sdk/permissions';
import {
  isProblemContentQuery,
  useProblemPresentationPolicy,
} from '@broccoli/web-sdk/problem';
import { useQueryClient } from '@tanstack/react-query';
import { useLayoutEffect } from 'react';

import { useMyMatch } from './hooks/useMyMatch';
import { matchPresentation } from './lib/presentation';

export function BracketPresentation({ contestId }: { contestId?: number }) {
  const { user } = useAuth();
  const mine = useMyMatch(contestId);
  const queryClient = useQueryClient();
  const staff = user?.permissions.includes(SUBMISSION_VIEW_ALL) ?? false;
  const view = matchPresentation(mine?.match, mine?.side);
  const ids = JSON.stringify(view.allowedProblemIds);
  const viewerId = user?.id;
  useProblemPresentationPolicy(
    `codelink-bracket:${contestId}`,
    contestId && viewerId && !staff ? { contestId, viewerId, ...view } : null,
  );

  // Remove statements and samples when leaving a phase or the contest layout.
  // This also removes the same problems' standalone-route cache. Editor drafts
  // live separately and are deliberately retained.
  useLayoutEffect(() => {
    if (!contestId || !viewerId || staff) return;
    const allowed = new Set<number>(JSON.parse(ids));
    const purge = (leaving: boolean) => {
      const filter = {
        predicate: (query: { queryKey: readonly unknown[] }) => {
          const key = query.queryKey;
          return (
            isProblemContentQuery(key, viewerId) &&
            (leaving
              ? key[2] === contestId || allowed.has(key[1] as number)
              : key[2] === contestId && !allowed.has(key[1] as number))
          );
        },
      };
      void queryClient.cancelQueries(filter);
      queryClient.removeQueries(filter);
    };
    purge(false);
    return () => purge(true);
  }, [contestId, viewerId, staff, ids, queryClient]);
  return null;
}
