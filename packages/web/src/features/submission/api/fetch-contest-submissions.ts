import type { ApiClient } from '@broccoli/web-sdk/api';
import type { ServerTableParams } from '@broccoli/web-sdk/hooks';
import type {
  SubmissionStatus,
  SubmissionSummary,
} from '@broccoli/web-sdk/submission';

import type { SubmissionListPage } from '@/features/submission/api/fetch-submissions';

export async function fetchContestSubmissions(
  apiClient: ApiClient,
  params: ServerTableParams & {
    contestId: number;
    problemId?: number | null;
    language?: string | null;
    status?: SubmissionStatus | null;
    userId?: number | undefined;
  },
): Promise<SubmissionListPage> {
  const { data, error } = await apiClient.GET('/contests/{id}/submissions', {
    params: {
      path: { id: params.contestId },
      query: {
        page: params.page,
        per_page: params.per_page,
        sort_by: params.sort_by,
        sort_order: params.sort_order,
        user_id: params.userId,
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

export async function fetchAllContestSubmissions(
  apiClient: ApiClient,
  params: {
    contestId: number;
    userId?: number | undefined;
  },
): Promise<SubmissionSummary[]> {
  const all: SubmissionSummary[] = [];
  let page = 1;
  let hasMore = true;

  while (hasMore) {
    const { data, error } = await apiClient.GET('/contests/{id}/submissions', {
      params: {
        path: { id: params.contestId },
        query: {
          page,
          per_page: undefined,
          sort_by: 'created_at',
          sort_order: 'desc',
          user_id: params.userId,
        },
      },
    });

    if (error) throw error;

    all.push(...data.data);
    hasMore = data.pagination.has_more;
    page += 1;
  }

  return all;
}
