import { parseApiError, serverNow, useApiClient } from '@broccoli/web-sdk/api';
import { useIdempotentRequest } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { toast } from 'sonner';

type ContestEnrollState = {
  is_public: boolean;
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
  const sendIdempotent = useIdempotentRequest();

  const enrollMutation = useMutation({
    mutationFn: async () => {
      const { error } = await sendIdempotent({ contestId }, (key) =>
        apiClient.POST('/contests/{id}/register', {
          headers: { 'Idempotency-Key': key },
          params: { path: { id: contestId } },
        }),
      );
      if (error) throw error;
    },
    onSuccess: () => {
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
  const hasEnded = contest ? serverNow() >= Date.parse(contest.end_time) : true;

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
    !hasEnded;

  return {
    canShowEnrollCard,
    canShowUnregisterButton,
    enroll: () => enrollMutation.mutate(),
    unregister: () => unregisterMutation.mutate(),
    isPending: enrollMutation.isPending,
    isUnregistering: unregisterMutation.isPending,
  };
}
