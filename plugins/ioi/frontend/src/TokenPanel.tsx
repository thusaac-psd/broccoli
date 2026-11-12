import { useServerNow, useServerTimeReached } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useMemo, useState } from 'react';

import { useIoiApi } from './hooks/useIoiApi';
import { useIsIoiContest } from './hooks/useIsIoiContest';
import type { TokenStatusResponse } from './types';

interface TokenPanelProps {
  submission?: {
    id: number;
    status: string;
    contest_id?: number | null;
  } | null;
  contestId?: number;
}

function formatCountdown(totalSeconds: number) {
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${seconds.toString().padStart(2, '0')}`;
}

export function TokenPanel({ submission, contestId }: TokenPanelProps) {
  const cId = contestId ?? submission?.contest_id ?? undefined;
  const { isIoi, contestInfo, isLoading: guardLoading } = useIsIoiContest(cId);
  const api = useIoiApi();
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const [isUsing, setIsUsing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmingSubmissionId, setConfirmingSubmissionId] = useState<
    number | null
  >(null);

  const tokenMode = contestInfo?.token_mode;
  const showTokens = isIoi && tokenMode && tokenMode !== 'none';

  const { data: tokenStatus } = useQuery({
    queryKey: ['ioi-token-status', cId],
    enabled: !!cId && showTokens === true,
    queryFn: async (): Promise<TokenStatusResponse | null> => {
      try {
        return await api.getTokenStatus(cId!);
      } catch (error) {
        if (
          error instanceof Error &&
          'status' in error &&
          (error.status === 401 || error.status === 403 || error.status === 404)
        ) {
          return null;
        }
        throw error;
      }
    },
    refetchInterval: 60000,
    retry: false,
  });

  const countdownTargetMs = useMemo(() => {
    if (!tokenStatus) return null;
    if (tokenStatus.mode !== 'regenerating' || !tokenStatus.next_regen_at) {
      return null;
    }
    const parsed = Date.parse(tokenStatus.next_regen_at);
    return Number.isNaN(parsed) ? null : parsed;
  }, [tokenStatus]);

  // The regeneration time is the server's, so the countdown runs on the
  // server clock; it ticks only while there is something to count down.
  const now = useServerNow(countdownTargetMs === null ? undefined : 1000);
  const countdownExpired = useServerTimeReached(countdownTargetMs);
  const remainingSeconds =
    countdownTargetMs === null
      ? null
      : Math.max(0, Math.ceil((countdownTargetMs - now) / 1000));
  const countdownLabel =
    remainingSeconds !== null && remainingSeconds > 0
      ? formatCountdown(remainingSeconds)
      : null;

  // Once per regeneration time: a status still showing a past time is left to
  // the regular refetch rather than asked for again every second.
  useEffect(() => {
    if (!cId || !countdownExpired) return;
    void queryClient.invalidateQueries({ queryKey: ['ioi-token-status', cId] });
  }, [cId, countdownExpired, queryClient]);

  if (guardLoading || !isIoi || !showTokens) return null;
  if (!tokenStatus) return null;

  const canUseToken =
    submission &&
    submission.status === 'Judged' &&
    tokenStatus.available > 0 &&
    !tokenStatus.tokened_submission_ids.includes(submission.id);

  const alreadyTokened =
    submission && tokenStatus.tokened_submission_ids.includes(submission.id);

  const handleUseToken = async () => {
    if (!submission || !cId) return;
    setIsUsing(true);
    setError(null);
    try {
      await api.useToken(cId, submission.id);
      queryClient.invalidateQueries({ queryKey: ['ioi-token-status', cId] });
      queryClient.invalidateQueries({
        queryKey: ['ioi-subtask-scores', cId, submission.id],
      });
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : 'Failed to use token');
    } finally {
      setIsUsing(false);
    }
  };

  const dots = [];
  const showDots = tokenStatus.total <= 20;
  if (showDots) {
    for (let i = 0; i < tokenStatus.total; i++) {
      const isAvailable = i < tokenStatus.available;
      dots.push(
        <span
          key={i}
          className={cn(
            'inline-block w-2.5 h-2.5 rounded-full transition-all duration-200',
            isAvailable
              ? 'bg-emerald-500 border-2 border-emerald-500'
              : 'bg-transparent border-2 border-border',
          )}
        />,
      );
    }
  }

  return (
    <div className="rounded-lg border border-border p-4 bg-card">
      <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground mb-3">
        {t('ioi.tokenPanel.title')}
      </div>

      {/* Token display */}
      {tokenStatus.total > 0 && (
        <>
          <div className="flex items-center gap-1 mb-2 flex-wrap">
            {showDots ? (
              dots
            ) : (
              <span className="font-mono tabular-nums text-xl font-bold text-foreground">
                {tokenStatus.available}
                <span className="opacity-40">/{tokenStatus.total}</span>
              </span>
            )}
            {showDots && (
              <span className="font-mono tabular-nums ml-2 text-[13px] text-foreground">
                {t('ioi.tokenPanel.available', {
                  available: tokenStatus.available,
                  total: tokenStatus.total,
                })}
              </span>
            )}
          </div>
          {countdownLabel && (
            <div className="font-mono tabular-nums mb-3 text-xs text-muted-foreground">
              {t('ioi.tokenPanel.nextTokenIn', { remaining: countdownLabel })}
            </div>
          )}
        </>
      )}

      {/* Use token button with confirmation */}
      {canUseToken && (
        <div className="relative">
          {confirmingSubmissionId === submission.id ? (
            <div className="p-2.5 px-3 rounded-md border border-amber-500/30 bg-amber-500/[0.06] flex flex-col gap-2">
              <div className="text-xs text-foreground leading-normal">
                {t('ioi.tokenPanel.confirmUse', {
                  id: submission.id,
                  remaining: tokenStatus.available - 1,
                })}
              </div>
              <div className="flex gap-2">
                <Button
                  size="sm"
                  className="flex-1"
                  onClick={() => {
                    setConfirmingSubmissionId(null);
                    handleUseToken();
                  }}
                  disabled={isUsing}
                >
                  {isUsing
                    ? t('ioi.tokenPanel.usingToken')
                    : t('ioi.tokenPanel.confirm')}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  className="flex-1"
                  onClick={() => setConfirmingSubmissionId(null)}
                >
                  {t('ioi.tokenPanel.cancel')}
                </Button>
              </div>
            </div>
          ) : (
            <Button
              className="w-full"
              onClick={() => setConfirmingSubmissionId(submission.id)}
            >
              {t('ioi.tokenPanel.useToken')}
            </Button>
          )}
        </div>
      )}

      {alreadyTokened && (
        <div className="py-1.5 px-2.5 rounded text-xs text-emerald-500 bg-emerald-500/10 text-center">
          {t('ioi.tokenPanel.alreadyUsed')}
        </div>
      )}

      {error && (
        <div className="mt-2 py-1.5 px-2.5 rounded text-xs text-red-500 bg-red-500/10">
          {error}
        </div>
      )}

      {/* Tokened submissions list */}
      {tokenStatus.tokened_submission_ids.length > 0 && (
        <div className="mt-3">
          <div className="text-[11px] text-muted-foreground mb-1">
            {t('ioi.tokenPanel.tokenedSubmissions')}
          </div>
          <div className="flex gap-1.5 flex-wrap">
            {tokenStatus.tokened_submission_ids.map((sid: number) => (
              <span
                key={sid}
                className="font-mono tabular-nums px-2 py-0.5 rounded text-[11px] bg-muted text-foreground"
              >
                #{sid}
              </span>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
