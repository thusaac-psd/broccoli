import { usePluginApi } from '@broccoli/web-sdk/api';
import { useMemo } from 'react';

import { buildAdminJobsQueryParams } from '../lib/adminJobsQuery';
import type {
  AdminJobsQuery,
  ArbitraryJobInput,
  ListResponse,
  PagedResponse,
  PrintJob,
  PrintStation,
  SubmitResult,
} from '../types';

const ROUTES = '/api/plugins/print';

export function usePrintApi() {
  const api = usePluginApi('print');

  return useMemo(
    () => ({
      // Contestant
      printSubmission: (contestId: number, submissionId: number) =>
        api.post<SubmitResult>(
          `${ROUTES}/contests/${contestId}/submissions/${submissionId}`,
        ),

      printArbitrary: ({ contest_id, ...input }: ArbitraryJobInput) =>
        api.post<SubmitResult>(
          contest_id === undefined
            ? `${ROUTES}/jobs`
            : `${ROUTES}/contests/${contest_id}/jobs`,
          input,
        ),

      myJobs: (contestId?: number) =>
        api.get<ListResponse<PrintJob>>(
          `${ROUTES}/jobs/mine${contestId ? `?contest_id=${contestId}` : ''}`,
        ),

      // Staff
      adminListJobs: (q: AdminJobsQuery) =>
        api.get<PagedResponse<PrintJob>>(
          `${ROUTES}/admin/jobs?${buildAdminJobsQueryParams(q).toString()}`,
        ),

      approveJob: (id: number) =>
        api.post(`${ROUTES}/admin/jobs/${id}/approve`),
      reprintJob: (id: number) =>
        api.post(`${ROUTES}/admin/jobs/${id}/reprint`),
      cancelJob: (id: number) => api.post(`${ROUTES}/admin/jobs/${id}/cancel`),

      listStations: () =>
        api.get<ListResponse<PrintStation>>(`${ROUTES}/admin/stations`),

      getJob: (id: number) =>
        api.get<{
          data: {
            id: number;
            source: string;
            language: string;
            filename: string;
            username: string;
            display_name: string | null;
          };
        }>(`${ROUTES}/admin/jobs/${id}`),

      pinJob: (id: number, printer: string | null) =>
        api.post(`${ROUTES}/admin/jobs/${id}/pin`, printer ? { printer } : {}),
    }),
    [api],
  );
}
