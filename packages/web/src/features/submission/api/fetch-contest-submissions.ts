import type { ApiClient } from '@broccoli/web-sdk/api';
import type {
  SubmissionStatus,
  SubmissionSummary,
} from '@broccoli/web-sdk/submission';

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
    userId?: number | undefined;
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
  let cursor: string | null = null;

  for (;;) {
    const page = await fetchContestSubmissionPage(
      apiClient,
      params.contestId,
      params.userId,
      cursor,
    );
    all.push(...page.data);
    cursor = page.pagination.next_cursor ?? null;
    if (!cursor) return all;
  }
}

async function fetchContestSubmissionPage(
  apiClient: ApiClient,
  contestId: number,
  userId: number | undefined,
  cursor: string | null,
): Promise<SubmissionListPage> {
  const { data, error } = await apiClient.GET('/contests/{id}/submissions', {
    params: {
      path: { id: contestId },
      query: {
        ...(cursor ? { cursor } : {}),
        sort_by: 'created_at',
        sort_order: 'desc',
        user_id: userId,
      },
    },
  });

  if (error) throw error;
  return data;
}
