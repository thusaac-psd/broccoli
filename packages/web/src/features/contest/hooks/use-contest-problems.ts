import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuth, useAuthReady, viewerCacheKey } from '@broccoli/web-sdk/auth';
import { CONTEST_MANAGE } from '@broccoli/web-sdk/permissions';
import { useProblemPresentation } from '@broccoli/web-sdk/problem';
import { useQuery } from '@tanstack/react-query';

import { fetchContestProblemList } from '@/features/contest/api/fetch-contest-problem-list';
import { useContestInfo } from '@/features/contest/hooks/use-contest-info';
import { problemListRefetchInterval } from '@/features/contest/utils/problem-refresh';
import { phaseHasStarted } from '@/features/contest/utils/status';

/**
 * The problems of `contestId` the viewer may see. Every view of the list
 * shares this query, so all of them refresh together: periodically while
 * the contest runs (see `problemListRefetchInterval`).
 *
 * The server lists a contest's problems only once it has started, unless the
 * viewer manages contests. So the list is fetched once the contest is known
 * and open to this viewer, and the moment it opens (when it typically goes
 * from hidden to full); `awaitingStart` says the viewer waits for that.
 */
export function useContestProblems(contestId: number | null | undefined) {
  const apiClient = useApiClient();
  const authReady = useAuthReady();
  const { user } = useAuth();
  const presentation = useProblemPresentation(contestId, user?.id);
  const id = contestId ?? NaN;
  const { contest } = useContestInfo(id);
  const canManage = !!user?.permissions.includes(CONTEST_MANAGE);
  const awaitingStart =
    !!contest && !phaseHasStarted(contest.phase) && !canManage;

  const query = useQuery({
    queryKey: ['contest-problems', contestId, viewerCacheKey(user)],
    enabled: authReady && !!contest && !awaitingStart,
    queryFn: () => fetchContestProblemList(apiClient, id),
    refetchInterval: () => problemListRefetchInterval(contest),
  });

  return {
    ...query,
    awaitingStart,
    data:
      presentation.allowedProblemIds === null
        ? query.data
        : query.data?.filter((problem) =>
            presentation.allowedProblemIds!.includes(problem.problem_id),
          ),
  };
}
