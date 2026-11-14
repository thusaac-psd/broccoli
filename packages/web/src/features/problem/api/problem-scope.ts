import type { ApiClient } from '@broccoli/web-sdk/api';

/**
 * Problem reads scoped to the contest they come from. Inside a contest the
 * problem page reads through that contest's routes, so the server decides
 * visibility for this one contest; another contest holding the same problem
 * cannot hide it here. Outside a contest the standalone routes are used.
 */

export async function fetchProblem(
  apiClient: ApiClient,
  problemId: number,
  contestId: number | undefined,
  signal?: AbortSignal,
) {
  const { data, error } =
    contestId === undefined
      ? await apiClient.GET('/problems/{id}', {
          params: { path: { id: problemId } },
          signal,
        })
      : await apiClient.GET('/contests/{id}/problems/{problem_id}', {
          params: { path: { id: contestId, problem_id: problemId } },
          signal,
        });
  if (error) throw error;
  return data;
}

/** One sample test case, or `null` when it cannot be read. */
export async function fetchSampleCase(
  apiClient: ApiClient,
  problemId: number,
  contestId: number | undefined,
  tcId: number,
  signal?: AbortSignal,
) {
  const { data, error } =
    contestId === undefined
      ? await apiClient.GET('/problems/{id}/test-cases/{tc_id}', {
          params: { path: { id: problemId, tc_id: tcId } },
          signal,
        })
      : await apiClient.GET(
          '/contests/{id}/problems/{problem_id}/samples/{tc_id}',
          {
            params: {
              path: { id: contestId, problem_id: problemId, tc_id: tcId },
            },
            signal,
          },
        );
  if (error) throw error;
  return data;
}

/**
 * Point a statement's link or image at the contest route when it is one of
 * this problem's own attachments. Any other URL is returned unchanged.
 */
export function scopeAttachmentUrl(
  apiBasePath: string,
  url: string | undefined,
  problemId: number,
  contestId: number | undefined,
): string | undefined {
  if (url === undefined || contestId === undefined) return url;
  const standalone = `${apiBasePath}/problems/${problemId}/attachments/`;
  if (!url.startsWith(standalone)) return url;
  return `${apiBasePath}/contests/${contestId}/problems/${problemId}/attachments/${url.slice(standalone.length)}`;
}
