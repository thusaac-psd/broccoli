import { type ApiSchemas, useApiClient } from '@broccoli/web-sdk/api';
import { pollInterval } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import { toast } from 'sonner';

/**
 * Drives the contest cache pre-warm: starts a warm job and polls per-worker
 * status while any judge is still warming. Polling stops once every live judge
 * has settled (complete or failed).
 */
export function useContestPrewarm(contestId: number, active: boolean) {
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const [jobId, setJobId] = useState<string | null>(null);

  const startMutation = useMutation({
    mutationFn: async () => {
      const { data, error } = await apiClient.POST('/contests/{id}/prewarm', {
        params: { path: { id: contestId } },
      });
      if (error) throw error;
      // Failed responses without a JSON body (401/403/404) surface as neither
      // `error` nor `data`; treat them as errors instead of crashing onSuccess.
      if (!data) throw new Error('Prewarm start returned no response body');
      return data;
    },
    onSuccess: (data) => {
      setJobId(data.job_id);
      queryClient.invalidateQueries({
        queryKey: ['prewarm-status', contestId],
      });
    },
    onError: () => {
      toast.error(t('warm.startError'));
    },
  });

  const statusQuery = useQuery({
    queryKey: ['prewarm-status', contestId, jobId],
    enabled: active,
    queryFn: async () => {
      const { data, error } = await apiClient.GET(
        '/contests/{id}/prewarm/status',
        {
          params: {
            path: { id: contestId },
            query: jobId ? { job: jobId } : {},
          },
        },
      );
      if (error) throw error;
      return data;
    },
    // Poll while a warm is starting or any live judge is still in flight.
    refetchInterval: pollInterval<ApiSchemas['WarmStatusResponse']>((data) => {
      if (startMutation.isPending) return 1200;
      if (!data) return 1500;
      const inFlight = data.workers.some(
        (w) => w.state === 'warming' || w.state === 'pending',
      );
      return inFlight ? 1500 : false;
    }),
  });

  return {
    start: () => startMutation.mutate(),
    isStarting: startMutation.isPending,
    status: statusQuery.data,
    isLoadingStatus: statusQuery.isLoading && active,
  };
}
