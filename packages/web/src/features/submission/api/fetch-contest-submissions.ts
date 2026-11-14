import type { ApiClient } from '@broccoli/web-sdk/api';
import type { SubmissionStatus } from '@broccoli/web-sdk/submission';

import type {
  SubmissionListPage,
  SubmissionPageParams,
} from '@/features/submission/api/fetch-submissions';

export async function fetchContestSubmissions(
  apiClient: ApiClient,
  params: SubmissionPageParams & {
    contestId: number;
    problemId?: number | null;
    language?: string | null;
    status?: SubmissionStatus | null;
  },
): Promise<SubmissionListPage> {
  const { data, error } = await apiClient.GET('/contests/{id}/submissions', {
    params: {
      path: { id: params.contestId },
      query: {
        ...(params.cursor ? { cursor: params.cursor } : {}),
        per_page: params.per_page,
        sort_by: params.sort_by,
        sort_order: params.sort_order,
        ...(params.problemId ? { problem_id: params.problemId } : {}),
        ...(params.language ? { language: params.language } : {}),
        ...(params.status ? { status: params.status } : {}),
      },
    },
  });

  if (error) throw error;

  return {
    data: data.data,
    pagination: data.pagination,
  };
}
