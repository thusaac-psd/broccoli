import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuth, viewerCacheKey } from '@broccoli/web-sdk/auth';
import { useProblemPresentation } from '@broccoli/web-sdk/problem';
import { useQuery } from '@tanstack/react-query';

export interface ProblemInfo {
  label: string;
  title: string;
}

/**
 * The contest's problems keyed by id, from the host's visibility-filtered
 * list: a problem this viewer may not see is simply absent, so callers fall
 * back to a neutral placeholder rather than leaking anything.
 */
export function useContestProblems(contestId: number | undefined) {
  const apiClient = useApiClient();
  const { user } = useAuth();
  const { allowedProblemIds } = useProblemPresentation(contestId, user?.id);
  const { data } = useQuery({
    queryKey: ['codelink-bracket-problems', contestId, viewerCacheKey(user)],
    enabled: !!contestId,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/contests/{id}/problems', {
        params: { path: { id: contestId as number } },
      });
      if (error || !data) return [];
      return data;
    },
  });
  const list = (data ?? []).filter(
    (problem) =>
      allowedProblemIds === null ||
      allowedProblemIds.includes(problem.problem_id),
  );
  const byId = new Map<number, ProblemInfo>(
    list.map((p) => [p.problem_id, { label: p.label, title: p.problem_title }]),
  );
  return { list, byId };
}

/** Enrolled contestants, for seeding. Staff only (the host enforces it). */
export function useParticipants(contestId: number, enabled: boolean) {
  const apiClient = useApiClient();
  const { user } = useAuth();
  return useQuery({
    queryKey: [
      'codelink-bracket-participants',
      contestId,
      viewerCacheKey(user),
    ],
    enabled,
    queryFn: async () => {
      const { data, error } = await apiClient.GET(
        '/contests/{id}/participants',
        { params: { path: { id: contestId } } },
      );
      if (error || !data) throw new Error('participants');
      return data.filter((p) => !p.is_deleted);
    },
  });
}

/** One player's latest contest submissions, newest first. */
export function usePlayerSubmissions(
  contestId: number,
  userId: number,
  enabled: boolean,
) {
  const apiClient = useApiClient();
  const { user } = useAuth();
  return useQuery({
    queryKey: [
      'codelink-bracket-submissions',
      contestId,
      userId,
      viewerCacheKey(user),
    ],
    enabled,
    refetchInterval: 5_000,
    queryFn: async () => {
      const { data, error } = await apiClient.GET(
        '/contests/{id}/submissions',
        {
          params: {
            path: { id: contestId },
            query: {
              user_id: userId,
              per_page: 20,
              sort_by: 'created_at',
              sort_order: 'desc',
            },
          },
        },
      );
      if (error || !data) throw new Error('submissions');
      return data.data;
    },
  });
}
