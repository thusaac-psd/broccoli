/**
 * Shows cooldown timer status on the problem detail sidebar.
 */
import { usePluginApi } from '@broccoli/web-sdk/api';
import { useAuth, useAuthReady, viewerCacheKey } from '@broccoli/web-sdk/auth';
import { pollInterval, useQueryState } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useSubmitGate } from '@broccoli/web-sdk/submission';
import { QueryErrorState, RefreshFailedNotice } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef } from 'react';

import { useSecondsLeft } from './lib/countdown';

interface Props {
  submission?: { id: number; status: string } | null;
  contestId?: number;
  problemId?: number;
}

/** The status route's answer. */
interface CooldownStatusData {
  enabled: boolean;
  cooldown_seconds: number;
  /** Seconds until the gate lets a submission pass, by the gate's own rule. */
  wait_seconds: number;
  source?: string;
}

/** The answer, with the monotonic clock reading it arrived at. */
interface CooldownStatusRead extends CooldownStatusData {
  receivedAt: number;
}

const ROUTES = '/api/plugins/cooldown';

export function CooldownStatus({ submission, contestId, problemId }: Props) {
  const api = usePluginApi('cooldown');
  const { user } = useAuth();
  const authReady = useAuthReady();
  const { t } = useTranslation();
  const queryClient = useQueryClient();

  const viewerKey = viewerCacheKey(user);
  const query = useQuery({
    queryKey: ['cooldown-status', contestId ?? null, problemId, viewerKey],
    enabled: authReady && !!user && problemId !== undefined,
    queryFn: async (): Promise<CooldownStatusRead> => {
      const path = contestId
        ? `${ROUTES}/contests/${contestId}/problems/${problemId}/status`
        : `${ROUTES}/problems/${problemId}/status`;
      const status = await api.get<CooldownStatusData>(path);
      return { ...status, receivedAt: performance.now() };
    },
    // Ask again once the wait is over, so the server confirms it.
    refetchInterval: pollInterval<CooldownStatusRead>((status) =>
      status && status.wait_seconds > 0 ? status.wait_seconds * 1000 : false,
    ),
  });

  const view = useQueryState(query);
  const { data } = query;

  // A new submission of this problem starts a new wait.
  const submissionId = submission?.id;
  const seenSubmission = useRef(submissionId);
  useEffect(() => {
    if (seenSubmission.current === submissionId) return;
    seenSubmission.current = submissionId;
    void queryClient.invalidateQueries({
      queryKey: ['cooldown-status', contestId ?? null, problemId, viewerKey],
    });
  }, [queryClient, submissionId, contestId, problemId, viewerKey]);

  const remaining = useSecondsLeft(
    data?.wait_seconds ?? 0,
    data?.receivedAt ?? 0,
  );

  const isCooldownBlocked = data?.enabled === true && remaining > 0;
  useSubmitGate('cooldown', isCooldownBlocked, t('cooldown.cooldownActive'));

  if (!problemId) return null;
  if (view.status === 'error') {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <QueryErrorState
          error={view.error}
          retry={view.retry}
          retrying={view.retrying}
        />
      </div>
    );
  }
  if (!data) return null;

  if (data.enabled === false || data.cooldown_seconds === 0) return null;

  const isReady = remaining === 0;
  const pct =
    ((data.cooldown_seconds - remaining) / data.cooldown_seconds) * 100;

  return (
    <div className="rounded-lg border border-border p-4 bg-card">
      <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground mb-3">
        {t('cooldown.cooldown')}
      </div>

      <div className="flex items-center gap-2">
        {/* Status dot */}
        <span
          className={cn(
            'inline-block w-2 h-2 rounded-full shrink-0',
            isReady ? 'bg-emerald-500' : 'bg-amber-500',
          )}
        />

        {isReady ? (
          <span className="text-[13px] text-emerald-500 font-medium">
            {t('cooldown.ready')}
          </span>
        ) : (
          <span className="font-mono tabular-nums text-[13px] text-amber-500 font-medium">
            {t('cooldown.waitShort', { seconds: remaining })}
          </span>
        )}
      </div>

      {/* Progress bar when cooling down */}
      {!isReady && data.cooldown_seconds > 0 && (
        <div className="mt-2.5 h-1 rounded-sm bg-muted overflow-hidden">
          <div
            className="h-full rounded-sm bg-amber-500 transition-[width] duration-1000 ease-linear"
            style={{ width: `${pct}%` }}
          />
        </div>
      )}

      <div className="mt-2 text-[11px] text-muted-foreground">
        {t('cooldown.betweenSubmissions', { seconds: data.cooldown_seconds })}
        {data.source && data.source !== 'default' && (
          <span className="ml-1 opacity-60">
            ({t(`cooldown.source.${data.source}`)})
          </span>
        )}
      </div>
      {view.status === 'data' && view.refreshError != null && (
        <RefreshFailedNotice retry={view.retry} className="mt-2" />
      )}
    </div>
  );
}
