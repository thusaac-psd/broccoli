import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  isTerminalStatus,
  type SubmissionStatus,
  type Verdict,
} from '@broccoli/web-sdk/submission';
import { Badge } from '@broccoli/web-sdk/ui';
import { formatKibibytes } from '@broccoli/web-sdk/utils';
import { AlertTriangle, Loader2, Pin } from 'lucide-react';

import { useSubmissionNavigation } from '@/features/submission/hooks/use-submission-navigation';
import type { SubmissionEntry } from '@/features/submission/hooks/use-submissions';
import { getVerdictBadge } from '@/features/submission/utils/verdict';

interface Props {
  /** All entries belonging to one fan-out group, in any order. */
  entries: SubmissionEntry[];
  linkBuilder?: (submissionId: number) => string;
}

interface Row {
  workerId: string;
  entry: SubmissionEntry;
  status: SubmissionStatus;
  verdict: Verdict | null;
  timeUsed: number | null;
  memoryUsed: number | null;
}

function buildRow(entry: SubmissionEntry): Row {
  const sub = entry.submission;
  return {
    workerId: entry.targetWorkerId ?? '?',
    entry,
    status: (sub?.status ?? 'Pending') as SubmissionStatus,
    verdict: (sub?.result?.verdict as Verdict | null) ?? null,
    timeUsed: sub?.result?.time_used ?? null,
    memoryUsed: sub?.result?.memory_used ?? null,
  };
}

export function PinnedSubmissionGroup({ entries, linkBuilder }: Props) {
  const { t } = useTranslation();
  const { openSubmission } = useSubmissionNavigation();

  const rows = entries
    .map(buildRow)
    .sort((a, b) => a.workerId.localeCompare(b.workerId));

  const allTerminal = rows.every((r) => isTerminalStatus(r.status));
  const verdictsSeen = new Set(rows.map((r) => r.verdict ?? r.status));
  const diverges = allTerminal && verdictsSeen.size > 1;
  const allAccepted =
    allTerminal &&
    rows.every((r) => r.verdict === 'Accepted' && r.status === 'Judged');

  return (
    <section
      className={`rounded-lg border p-3 ${
        diverges
          ? 'border-amber-500/60 bg-amber-50/40 dark:bg-amber-950/20'
          : allAccepted
            ? 'border-emerald-500/60 bg-emerald-50/40 dark:bg-emerald-950/20'
            : 'bg-card'
      }`}
    >
      <header className="flex items-center justify-between gap-2 pb-2">
        <h3 className="inline-flex items-center gap-2 text-sm font-semibold">
          <Pin className="h-3.5 w-3.5" />
          {t('pinnedGroup.title', { count: String(rows.length) })}
        </h3>
        {diverges && (
          <Badge
            variant="outline"
            className="gap-1 border-amber-500/60 text-amber-700 dark:text-amber-300"
            title={t('pinnedGroup.divergesHint')}
          >
            <AlertTriangle className="h-3 w-3" />
            {t('pinnedGroup.diverges')}
          </Badge>
        )}
        {allAccepted && (
          <Badge
            variant="outline"
            className="border-emerald-500/60 text-emerald-700 dark:text-emerald-300"
          >
            {t('pinnedGroup.allAccepted')}
          </Badge>
        )}
      </header>

      <div className="space-y-1">
        {rows.map((row) => {
          const submissionId = row.entry.submission?.id ?? null;
          const canOpen = submissionId != null && !!linkBuilder;
          const { label, variant } = getVerdictBadge(
            row.verdict,
            row.status,
            t,
          );
          const isPolling =
            row.entry.status === 'submitting' || row.entry.status === 'polling';
          return (
            <div
              key={row.entry.id}
              className={`grid grid-cols-[8rem_minmax(0,1fr)_auto_auto_auto] items-center gap-3 rounded-md border bg-muted/20 px-3 py-2 text-xs ${
                canOpen ? 'cursor-pointer hover:bg-muted/40' : ''
              }`}
              onClick={() =>
                canOpen && openSubmission(linkBuilder(submissionId as number))
              }
              role={canOpen ? 'link' : undefined}
              tabIndex={canOpen ? 0 : undefined}
              onKeyDown={
                canOpen
                  ? (e) => {
                      if (e.key === 'Enter' || e.key === ' ') {
                        e.preventDefault();
                        openSubmission(linkBuilder(submissionId as number));
                      }
                    }
                  : undefined
              }
            >
              <span
                className="font-mono text-[11px] truncate"
                title={row.workerId}
              >
                {row.workerId}
              </span>

              <div className="flex items-center gap-2">
                {isPolling ? (
                  <span className="inline-flex items-center gap-1 text-muted-foreground">
                    <Loader2 className="h-3 w-3 animate-spin" />
                    {row.status}
                  </span>
                ) : (
                  <Badge variant={variant} className="text-[10px]">
                    {label}
                  </Badge>
                )}
                {row.entry.status === 'error' && row.entry.error && (
                  <span className="text-rose-500 text-[10px] font-mono">
                    {row.entry.error.code}
                  </span>
                )}
              </div>

              <span className="font-mono text-[10px] text-muted-foreground tabular-nums">
                {row.timeUsed != null
                  ? t('result.timeValue', { value: row.timeUsed })
                  : '—'}
              </span>
              <span className="font-mono text-[10px] text-muted-foreground tabular-nums">
                {row.memoryUsed != null ? formatKibibytes(row.memoryUsed) : '—'}
              </span>
              <span className="font-mono text-[10px] text-muted-foreground tabular-nums">
                {submissionId != null ? `#${submissionId}` : ''}
              </span>
            </div>
          );
        })}
      </div>
    </section>
  );
}
