import type { ApiClient } from '@broccoli/web-sdk/api';
import type {
  SubmissionListPagination,
  SubmissionStatus,
  SubmissionSummary,
} from '@broccoli/web-sdk/submission';

export interface SubmissionListPage {
  data: SubmissionSummary[];
  pagination: SubmissionListPagination;
}

/** One cursor page of a submission list, in a fixed sort order. */
export interface SubmissionPageParams {
  /** `pagination.next_cursor` of the previous page; `null` for the first. */
  cursor: string | null;
  per_page: number;
  sort_by: 'created_at' | 'status';
  sort_order: 'asc' | 'desc';
}

export async function fetchSubmissions(
  apiClient: ApiClient,
  params: SubmissionPageParams & {
    q?: string | null;
    language?: string | null;
    status?: SubmissionStatus | null;
  },
): Promise<SubmissionListPage> {
  const { data, error } = await apiClient.GET('/submissions', {
    params: {
      query: {
        ...(params.cursor ? { cursor: params.cursor } : {}),
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
