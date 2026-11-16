/**
 * Shows cooldown timer status on the problem detail sidebar.
 */
import { usePluginApi } from '@broccoli/web-sdk/api';
import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useSubmitGate } from '@broccoli/web-sdk/submission';
import { cn } from '@broccoli/web-sdk/utils';
import { useCallback, useEffect, useRef, useState } from 'react';

interface Props {
  submission?: { id: number; status: string } | null;
  contestId?: number;
  problemId?: number;
}

interface CooldownStatusData {
  enabled?: boolean;
  cooldown_seconds: number;
  seconds_since_last: number | null;
  can_submit: boolean;
  source?: string;
}

const ROUTES = '/api/plugins/cooldown';

export function CooldownStatus({ submission, contestId, problemId }: Props) {
  const api = usePluginApi('cooldown');
  const { accessToken } = useAuth();
  const { t } = useTranslation();
  const [data, setData] = useState<CooldownStatusData | null>(null);
  const [remaining, setRemaining] = useState<number>(0);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const submissionId = submission?.id;

  const fetchStatus = useCallback(async () => {
    if (!problemId) return;
    const path = contestId
      ? `${ROUTES}/contests/${contestId}/problems/${problemId}/status`
      : `${ROUTES}/problems/${problemId}/status`;
    try {
      const d = await api.get<CooldownStatusData>(path);
      setData(d);

      // Compute initial remaining seconds
      if (d.cooldown_seconds === 0 || d.can_submit) {
        setRemaining(0);
      } else if (d.seconds_since_last !== null) {
        setRemaining(
          Math.max(0, d.cooldown_seconds - Math.max(0, d.seconds_since_last)),
        );
      } else {
        setRemaining(0);
      }
    } catch {
      // silent
    }
  }, [api, contestId, problemId]);

  // Fetch on mount, when submission changes, and when auth token becomes available
  useEffect(() => {
    if (!problemId || !accessToken) return;

    let cancelled = false;

    (async () => {
      await fetchStatus();
      if (cancelled) return;
    })();

    return () => {
      cancelled = true;
    };
  }, [contestId, problemId, submissionId, accessToken, fetchStatus]);

  const isCounting = remaining > 0;
  useEffect(() => {
    if (timerRef.current) clearInterval(timerRef.current);

    if (isCounting) {
      timerRef.current = setInterval(() => {
        setRemaining((prev) => {
          if (prev <= 1) {
            if (timerRef.current) clearInterval(timerRef.current);
            fetchStatus();
            return 0;
          }
          return prev - 1;
        });
      }, 1000);
    }

    return () => {
      if (timerRef.current) clearInterval(timerRef.current);
    };
  }, [fetchStatus, isCounting]);

  const isCooldownBlocked = data?.enabled === true && remaining > 0;
  useSubmitGate('cooldown', isCooldownBlocked, t('cooldown.cooldownActive'));

  if (!problemId || !data) return null;

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
    </div>
  );
}
