import { parseApiError, useApiClient } from '@broccoli/web-sdk/api';
import { useIdempotencyKey } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { toast } from 'sonner';

type ContestEnrollState = {
  is_public: boolean;
  start_time: string;
  end_time: string;
};

export function useContestEnroll({
  contestId,
  contest,
  canManageContest,
}: {
  contestId: number;
  contest?: ContestEnrollState;
  canManageContest: boolean;
}) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const { getKey, resetKey } = useIdempotencyKey();

  const enrollMutation = useMutation({
    mutationFn: async () => {
      const { error } = await apiClient.POST('/contests/{id}/register', {
        headers: { 'Idempotency-Key': getKey() },
        params: { path: { id: contestId } },
      });
      if (error) throw error;
    },
    onSuccess: () => {
      resetKey();
      toast.success(t('toast.contest.enrolled'));
      queryClient.invalidateQueries({ queryKey: ['contest', contestId] });
      queryClient.invalidateQueries({
        queryKey: ['contest-my-info', contestId],
      });
      queryClient.invalidateQueries({ queryKey: ['dashboard-contests'] });
    },
    onError: (error: unknown) => {
      if (parseApiError(error)?.code === 'CONFLICT') {
        toast.success(t('toast.contest.enrolled'));
        queryClient.invalidateQueries({ queryKey: ['contest', contestId] });
        queryClient.invalidateQueries({
          queryKey: ['contest-my-info', contestId],
        });
        return;
      }
      toast.error(t('toast.contest.enrollError'));
    },
  });

  const unregisterMutation = useMutation({
    mutationFn: async () => {
      const { error } = await apiClient.DELETE('/contests/{id}/register', {
        params: { path: { id: contestId } },
      });
      if (error) throw error;
    },
    onSuccess: () => {
      toast.success(t('toast.contest.unregistered'));
      queryClient.invalidateQueries({ queryKey: ['contest', contestId] });
      queryClient.invalidateQueries({
        queryKey: ['contest-my-info', contestId],
      });
      queryClient.invalidateQueries({ queryKey: ['dashboard-contests'] });
    },
    onError: () => {
      toast.error(t('toast.contest.unregisterError'));
    },
  });

  const { data: myInfo } = useQuery({
    queryKey: ['contest-my-info', contestId],
    enabled: !!contest && !canManageContest,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/contests/{id}/me', {
        params: { path: { id: contestId } },
      });
      if (error) throw error;
      return data;
    },
  });

  const hasMyInfo = myInfo !== undefined;
  const isRegistered = Boolean(myInfo?.is_registered);
  const hasEnded = contest
    ? Date.now() >= new Date(contest.end_time).getTime()
    : true;
  // The server refuses to unregister once the contest has started, because
  // standings are built from registrations.
  const hasStarted = contest
    ? Date.now() >= new Date(contest.start_time).getTime()
    : true;

  const canShowEnrollCard =
    !!contest &&
    hasMyInfo &&
    !canManageContest &&
    contest.is_public &&
    !isRegistered &&
    !hasEnded;

  const canShowUnregisterButton =
    !!contest &&
    hasMyInfo &&
    !canManageContest &&
    contest.is_public &&
    isRegistered &&
    !hasStarted;

  return {
    canShowEnrollCard,
    canShowUnregisterButton,
    enroll: () => enrollMutation.mutate(),
    unregister: () => unregisterMutation.mutate(),
    isPending: enrollMutation.isPending,
    isUnregistering: unregisterMutation.isPending,
  };
}
