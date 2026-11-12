import type { ApiClient } from '@broccoli/web-sdk/api';
import type { ServerTableParams } from '@broccoli/web-sdk/hooks';
import type {
  SubmissionListPagination,
  SubmissionStatus,
  SubmissionSummary,
} from '@broccoli/web-sdk/submission';

export interface SubmissionListPage {
  data: SubmissionSummary[];
  pagination: SubmissionListPagination;
}

export async function fetchSubmissions(
  apiClient: ApiClient,
  params: ServerTableParams & {
    q?: string | null;
    language?: string | null;
    status?: SubmissionStatus | null;
  },
): Promise<SubmissionListPage> {
  const { data, error } = await apiClient.GET('/submissions', {
    params: {
      query: {
        page: params.page,
        per_page: params.per_page,
        sort_by: params.sort_by,
        sort_order: params.sort_order,
        ...(params.q ? { q: params.q } : {}),
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
