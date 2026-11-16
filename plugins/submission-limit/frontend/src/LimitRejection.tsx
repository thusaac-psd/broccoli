/**
 * Submission-limit rejection wrapper for the `submission-result.rejection` slot.
 */
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge } from '@broccoli/web-sdk/ui';
import type { ReactNode } from 'react';

interface Props {
  error?: { code: string; message: string; details?: Record<string, unknown> };
  children?: ReactNode;
}

function getLimitCounts(
  details?: Record<string, unknown>,
): { used: number; total: number } | null {
  if (!details) return null;
  const used = details.submissions_made;
  const total = details.max_submissions;
  if (typeof used === 'number' && typeof total === 'number') {
    return { used, total };
  }
  return null;
}

export function LimitRejection({ error, children }: Props) {
  const { t } = useTranslation();

  if (!error || error.code !== 'submission-limit.SUBMISSION_LIMIT_EXCEEDED') {
    return <>{children}</>;
  }

  const counts = getLimitCounts(error.details);

  return (
    <div className="rounded-xl border border-border bg-card overflow-hidden h-full">
      {/* Header */}
      <div className="px-6 pt-5">
        <div className="flex items-center justify-between">
          <span className="text-base font-semibold text-foreground">
            {t('submission-limit.result')}
          </span>
          <Badge variant="destructive" className="rounded-full">
            {t('submission-limit.limitReached')}
          </Badge>
        </div>
      </div>

      {/* Content */}
      <div className="px-6 pb-6 pt-2 flex flex-col items-center gap-5">
        {/* Icon */}
        <div className="w-16 h-16 rounded-full bg-red-500/10 flex items-center justify-center">
          <svg
            width="32"
            height="32"
            viewBox="0 0 24 24"
            fill="none"
            stroke="#ef4444"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <circle cx="12" cy="12" r="10" />
            <line x1="15" y1="9" x2="9" y2="15" />
            <line x1="9" y1="9" x2="15" y2="15" />
          </svg>
        </div>

        {/* Message */}
        <div className="text-center max-w-[280px]">
          <p className="text-[13px] font-medium text-foreground">
            {t('submission-limit.submissionLimitReached')}
          </p>
          <p className="text-[11px] text-muted-foreground mt-1">
            {t('submission-limit.allUsed')}
          </p>
        </div>

        {/* Progress bar with count */}
        {counts && (
          <div className="w-full max-w-[240px]">
            <div className="h-2 rounded bg-muted overflow-hidden">
              <div className="h-full rounded bg-red-500 w-full" />
            </div>
            <div className="flex justify-between mt-2">
              <span className="font-mono tabular-nums text-[11px] text-muted-foreground">
                {t('submission-limit.usedCount', {
                  used: counts.used,
                  total: counts.total,
                })}
              </span>
              <span className="font-mono tabular-nums text-[11px] font-medium text-red-500">
                {t('submission-limit.zeroRemaining')}
              </span>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
