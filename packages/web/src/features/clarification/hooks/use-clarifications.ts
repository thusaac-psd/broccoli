import { useApiClient } from '@broccoli/web-sdk/api';
import type { CreateClarificationBody } from '@broccoli/web-sdk/clarification';
import { isAlreadyDone, useIdempotentRequest } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { toast } from 'sonner';

import { toastUnsettled } from '@/lib/idempotency-feedback';

import {
  createClarification,
  fetchClarifications,
  replyClarification,
  resolveClarification,
  toggleReplyPublic,
} from '../api/api';

/** An earlier send already posted it (its response was lost): a success. */
function treatAlreadyDoneAsPosted(error: unknown): null {
  if (isAlreadyDone(error)) return null;
  throw error;
}

/**
 * An earlier send may have posted it, or is still posting it. No refetch is
 * needed here: `onSettled` refetches the list, which shows whether it was.
 */
function reportedUnsettled(
  error: unknown,
  t: (key: string) => string,
): boolean {
  return toastUnsettled(error, t, {
    outcomeUnknownKey: 'clarification.outcomeUnknown',
  });
}

export function useClarifications(contestId: number, enabled: boolean) {
  const apiClient = useApiClient();
  return useQuery({
    queryKey: ['contest-clarifications', contestId],
    queryFn: () => fetchClarifications(apiClient, contestId),
    enabled,
    refetchInterval: 5000,
  });
}

export function useCreateClarification(contestId: number) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  // The key follows the clarification's content: a retry of the same text
  // reuses it, an edited one gets a fresh key.
  const sendIdempotent = useIdempotentRequest();
  return useMutation({
    mutationFn: (body: CreateClarificationBody) =>
      sendIdempotent({ contestId, body }, (key) =>
        createClarification(apiClient, contestId, body, key),
      ).catch(treatAlreadyDoneAsPosted),
    onSuccess: () => {
      toast.success(t('clarification.submitSuccess'));
    },
    onError: (error: Error) => {
      if (reportedUnsettled(error, t)) return;
      toast.error(error.message || t('clarification.submitError'));
    },
    onSettled: () => {
      queryClient.invalidateQueries({
        queryKey: ['contest-clarifications', contestId],
      });
    },
  });
}

export function useReplyClarification(contestId: number) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const sendIdempotent = useIdempotentRequest();
  return useMutation({
    mutationFn: (payload: { clarificationId: number; content: string }) =>
      sendIdempotent({ contestId, ...payload }, (key) =>
        replyClarification(
          apiClient,
          contestId,
          payload.clarificationId,
          { content: payload.content, is_public: false },
          key,
        ),
      ).catch(treatAlreadyDoneAsPosted),
    onSuccess: () => {
      toast.success(t('clarification.replySuccess'));
    },
    onError: (error: Error) => {
      if (reportedUnsettled(error, t)) return;
      toast.error(error.message || t('clarification.replyError'));
    },
    onSettled: () => {
      queryClient.invalidateQueries({
        queryKey: ['contest-clarifications', contestId],
      });
    },
  });
}

export function useResolveClarification(contestId: number) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (payload: { clarificationId: number; resolved: boolean }) =>
      resolveClarification(apiClient, contestId, payload.clarificationId, {
        resolved: payload.resolved,
      }),
    onSuccess: (_data, variables) => {
      toast.success(
        variables.resolved
          ? t('clarification.resolved')
          : t('clarification.reopened'),
      );
    },
    onError: (error: Error) => {
      toast.error(error.message || t('clarification.resolveError'));
    },
    onSettled: () => {
      queryClient.invalidateQueries({
        queryKey: ['contest-clarifications', contestId],
      });
    },
  });
}

export function useToggleReplyPublic(contestId: number) {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (payload: {
      clarificationId: number;
      replyId: number;
      includeQuestion?: boolean;
    }) =>
      toggleReplyPublic(
        apiClient,
        contestId,
        payload.clarificationId,
        payload.replyId,
        payload.includeQuestion,
      ),
    onSuccess: (data) => {
      toast.success(
        data.is_public
          ? t('clarification.madePublic')
          : t('clarification.madePrivate'),
      );
    },
    onError: (error: Error) => {
      toast.error(error.message || t('clarification.toggleError'));
    },
    onSettled: () => {
      queryClient.invalidateQueries({
        queryKey: ['contest-clarifications', contestId],
      });
    },
  });
}
