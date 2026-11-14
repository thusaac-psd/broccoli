import type { ApiClient } from '@broccoli/web-sdk/api';
import type { ContestProblem } from '@broccoli/web-sdk/contest';
import type { ServerTableParams } from '@broccoli/web-sdk/hooks';

/**
 * Fetches the problems attached to a contest, ordered by contest position
 * (label order). Rows are `ContestProblem` relationship records, NOT full
 * problem summaries: the endpoint only returns contest_id, label, position,
 * problem_id and problem_title. Callers must render contest-specific columns.
 *
 * The endpoint is not paginated or searchable server-side, so search is
 * applied client-side against the label and title.
 */
export async function fetchContestProblems(
  apiClient: ApiClient,
  params: ServerTableParams & { contestId: number },
) {
  const { data, error } = await apiClient.GET('/contests/{id}/problems', {
    params: {
      path: { id: params.contestId },
    },
  });

  if (error) throw error;

  const search = params.search?.trim().toLowerCase();
  const rows = data
    .filter(
      (p: ContestProblem) =>
        !search ||
        p.problem_title?.toLowerCase().includes(search) ||
        p.label.toLowerCase().includes(search),
    )
    .sort((a: ContestProblem, b: ContestProblem) => a.position - b.position);

  return {
    data: rows,
    pagination: {
      page: 1,
      per_page: rows.length,
      total: rows.length,
      total_pages: 1,
    },
  };
}
