/**
 * Shows submission count / limit on the problem detail sidebar.
 * Hidden when no limit is configured (unlimited) or plugin is not enabled.
 */
import { usePluginApi } from '@broccoli/web-sdk/api';
import { useAuth, useAuthReady, viewerCacheKey } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useSubmitGate } from '@broccoli/web-sdk/submission';
import { cn } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef } from 'react';

interface Props {
  submission?: { id: number; status: string } | null;
  contestId?: number;
  problemId?: number;
}

interface LimitStatus {
  enabled?: boolean;
  submissions_made: number;
  max_submissions: number;
  remaining: number | null;
  unlimited: boolean;
  source?: string;
}

const ROUTES = '/api/plugins/submission-limit';

export function SubmissionLimitStatus({
  submission,
  contestId,
  problemId,
}: Props) {
  const api = usePluginApi('submission-limit');
  const { user } = useAuth();
  const authReady = useAuthReady();
  const { t } = useTranslation();
  const queryClient = useQueryClient();

  const viewerKey = viewerCacheKey(user);
  const { data: status } = useQuery({
    queryKey: [
      'submission-limit-status',
      contestId ?? null,
      problemId,
      viewerKey,
    ],
    enabled: authReady && !!user && problemId !== undefined,
    queryFn: () =>
      api.get<LimitStatus>(
        contestId
          ? `${ROUTES}/contests/${contestId}/problems/${problemId}/status`
          : `${ROUTES}/problems/${problemId}/status`,
      ),
  });

  // A new submission of this problem uses up one more.
  const submissionId = submission?.id;
  const seenSubmission = useRef(submissionId);
  useEffect(() => {
    if (seenSubmission.current === submissionId) return;
    seenSubmission.current = submissionId;
    void queryClient.invalidateQueries({
      queryKey: [
        'submission-limit-status',
        contestId ?? null,
        problemId,
        viewerKey,
      ],
    });
  }, [queryClient, submissionId, contestId, problemId, viewerKey]);

  useSubmitGate(
    'submission-limit',
    status?.enabled === true && !status.unlimited && status.remaining === 0,
    t('submission-limit.reached'),
  );

  if (!problemId || !status) return null;

  if (status.enabled === false || status.unlimited) return null;

  const { submissions_made, max_submissions, remaining } = status;
  const pct = Math.min((submissions_made / max_submissions) * 100, 100);

  return (
    <div className="rounded-lg border border-border p-4 bg-card">
      <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground mb-3">
        {t('submission-limit.submissions')}
      </div>

      <div className="flex justify-between items-baseline mb-1.5">
        <span
          className={cn(
            'font-mono tabular-nums text-[13px]',
            remaining === 0 ? 'text-red-500' : 'text-foreground',
          )}
        >
          {submissions_made} / {max_submissions}
        </span>
        {remaining !== null && remaining > 0 && (
          <span className="text-[11px] text-muted-foreground">
            {t('submission-limit.remaining', { count: remaining })}
          </span>
        )}
        {remaining === 0 && (
          <span className="text-[11px] text-red-500 font-medium">
            {t('submission-limit.reached')}
          </span>
        )}
      </div>
      <div className="h-1 rounded-sm bg-muted overflow-hidden">
        <div
          className={cn(
            'h-full rounded-sm transition-[width] duration-300 ease-out',
            remaining === 0
              ? 'bg-red-500'
              : remaining !== null &&
                  remaining <= Math.ceil(max_submissions * 0.1)
                ? 'bg-amber-500'
                : 'bg-primary',
          )}
          style={{ width: `${pct}%` }}
        />
      </div>
      {status.source && status.source !== 'default' && (
        <div className="mt-1.5 text-[11px] text-muted-foreground opacity-60">
          ({t(`submission-limit.source.${status.source}`)})
        </div>
      )}
    </div>
  );
}
