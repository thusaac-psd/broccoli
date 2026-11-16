import { useErrorMessage } from '@broccoli/web-sdk/api';
import { useFormat, useTranslation } from '@broccoli/web-sdk/i18n';
import type {
  SubmissionStatus,
  SubmissionSummary,
  Verdict,
} from '@broccoli/web-sdk/submission';
import { Badge, QueryErrorState } from '@broccoli/web-sdk/ui';
import { formatKibibytes } from '@broccoli/web-sdk/utils';

import { useSubmissionNavigation } from '@/features/submission/hooks/use-submission-navigation';
import type { SubmissionEntry } from '@/features/submission/hooks/use-submissions';

import { getSubmissionScoreDisplay } from '../utils/score-display';
import { getVerdictBadge } from '../utils/verdict';
import { PinnedSubmissionGroup } from './PinnedSubmissionGroup';
import { SubmissionRejection } from './SubmissionResult';

const DEFAULT_VISIBLE_COUNT = 5;

interface RecentSubmissionOverviewProps {
  entries: SubmissionEntry[];
  history: SubmissionSummary[];
  /** Set when the earlier submissions could not be loaded. */
  historyError?: { error: unknown; retry: () => void } | null;
  isSubmitting?: boolean;
  visibleCount?: number;
  linkBuilder?: (submissionId: number) => string;
}

interface OverviewRow {
  key: string;
  submissionId: number | null;
  pendingOrder: number;
  status: SubmissionStatus;
  verdict: Verdict | null;
  score: number | null;
  createdAt: string | null;
  timeUsed: number | null;
  memoryUsed: number | null;
}

function getEntryStatus(entry: SubmissionEntry): SubmissionStatus {
  if (entry.submission) {
    return entry.submission.status;
  }

  switch (entry.status) {
    case 'submitting':
      return 'Pending';
    case 'polling':
      return 'Running';
    default:
      return 'Pending';
  }
}

function getScoreToneClass(variant: string): string {
  switch (variant) {
    case 'accepted':
      return 'text-emerald-600 dark:text-emerald-300';
    case 'wronganswer':
    case 'runtimeerror':
      return 'text-rose-600 dark:text-rose-300';
    case 'timelimitexceeded':
    case 'memorylimitexceeded':
      return 'text-amber-600 dark:text-amber-300';
    case 'secondary':
      return 'text-slate-600 dark:text-slate-300';
    default:
      return 'text-foreground';
  }
}

export function RecentSubmissionOverview({
  entries,
  history,
  historyError = null,
  isSubmitting,
  visibleCount = DEFAULT_VISIBLE_COUNT,
  linkBuilder,
}: RecentSubmissionOverviewProps) {
  const { t } = useTranslation();
  const errorMessage = useErrorMessage();
  const format = useFormat();
  const { openSubmission } = useSubmissionNavigation();

  // Bucket fan-out entries by groupKey so they render together as a single
  // comparison strip rather than N independent overview rows. Single-target
  // pins still create a 1-row group, which is fine - it shows the worker
  // label and avoids special-casing.
  const groups = new Map<string, SubmissionEntry[]>();
  const ungrouped: SubmissionEntry[] = [];
  for (const entry of entries) {
    if (entry.groupKey) {
      const arr = groups.get(entry.groupKey) ?? [];
      arr.push(entry);
      groups.set(entry.groupKey, arr);
    } else {
      ungrouped.push(entry);
    }
  }
  const groupedSubmissionIds = new Set<number>();
  for (const arr of groups.values()) {
    for (const e of arr) {
      if (e.submission) groupedSubmissionIds.add(e.submission.id);
    }
  }

  const rowsById = new Map<number, OverviewRow>();
  for (const item of history) {
    if (groupedSubmissionIds.has(item.id)) continue;
    rowsById.set(item.id, {
      key: `history-${item.id}`,
      submissionId: item.id,
      pendingOrder: 0,
      status: item.status,
      verdict: (item.verdict as Verdict | null) ?? null,
      score: item.score ?? null,
      createdAt: item.created_at,
      timeUsed: item.time_used ?? null,
      memoryUsed: item.memory_used ?? null,
    });
  }

  // A refused attempt is not a submission: it shows as the reason it was
  // refused, never as a row being judged. Entries are newest first.
  const rejection = ungrouped.find((entry) => entry.status === 'rejected');
  const unfollowable = ungrouped.find(
    (entry) => entry.status === 'error' && !entry.submission,
  );
  const notices = (
    <>
      {rejection && (
        <SubmissionRejection
          error={rejection.error}
          failure={t('toast.submission.error')}
        />
      )}
      {unfollowable && (
        <p role="alert" className="text-sm text-destructive py-2">
          {errorMessage(unfollowable.error, t('submissions.loadFailed'))}
        </p>
      )}
    </>
  );

  const pendingRows: OverviewRow[] = [];
  for (const entry of ungrouped) {
    if (entry.status === 'rejected') continue;
    if (!entry.submission) {
      if (entry.status === 'error') continue;
      pendingRows.push({
        key: `entry-${entry.id}`,
        submissionId: null,
        pendingOrder: entry.id,
        status: getEntryStatus(entry),
        verdict: null,
        score: null,
        createdAt: null,
        timeUsed: null,
        memoryUsed: null,
      });
      continue;
    }

    const sub = entry.submission;
    rowsById.set(sub.id, {
      key: `submission-${sub.id}`,
      submissionId: sub.id,
      pendingOrder: 0,
      status: sub.status,
      verdict: sub.result?.verdict ?? null,
      score: sub.result?.score ?? null,
      createdAt: sub.created_at,
      timeUsed: sub.result?.time_used ?? null,
      memoryUsed: sub.result?.memory_used ?? null,
    });
  }

  const mergedRows = [...pendingRows, ...Array.from(rowsById.values())]
    .sort((a, b) => {
      if (a.submissionId == null && b.submissionId == null) {
        return b.pendingOrder - a.pendingOrder;
      }
      if (a.submissionId == null) return -1;
      if (b.submissionId == null) return 1;
      const aTime = a.createdAt ? Date.parse(a.createdAt) : 0;
      const bTime = b.createdAt ? Date.parse(b.createdAt) : 0;
      return bTime - aTime;
    })
    .slice(0, visibleCount);

  const groupBlocks = Array.from(groups.entries());

  if (mergedRows.length === 0 && groupBlocks.length === 0) {
    return (
      <section className="rounded-lg border bg-card p-4">
        <h3 className="text-sm font-semibold">
          {t('result.latestOverviewTitle')}
        </h3>
        {notices}
        {historyError && !isSubmitting ? (
          <QueryErrorState
            className="py-4"
            error={historyError.error}
            retry={historyError.retry}
            failure={t('submissions.loadFailed')}
          />
        ) : (
          <p className="mt-2 text-sm text-muted-foreground">
            {isSubmitting
              ? t('result.judging')
              : t('result.latestOverviewEmpty')}
          </p>
        )}
      </section>
    );
  }

  const linkBuilderForGroup = linkBuilder
    ? (id: number) => linkBuilder(id)
    : undefined;

  return (
    <section className="rounded-lg border bg-card p-4">
      <h3 className="text-sm font-semibold">
        {t('result.latestOverviewTitle')}
      </h3>
      {notices}
      <div className="mt-3 space-y-2">
        {groupBlocks.map(([key, groupEntries]) => (
          <PinnedSubmissionGroup
            key={key}
            entries={groupEntries}
            linkBuilder={linkBuilderForGroup}
          />
        ))}
        {mergedRows.map((row) => {
          const { label, variant } = getVerdictBadge(
            row.verdict,
            row.status,
            t,
          );
          const scoreToneClass = getScoreToneClass(variant);
          const scoreDisplay = getSubmissionScoreDisplay(
            row.status,
            row.score,
            t,
          );
          const canOpen = row.submissionId != null && !!linkBuilder;

          return (
            <article
              key={row.key}
              className={`grid grid-cols-[auto_1fr] items-start gap-3 rounded-md border bg-muted/20 p-3 ${canOpen ? 'cursor-pointer transition-colors hover:bg-muted/30' : ''}`}
              onClick={
                canOpen
                  ? () =>
                      openSubmission(linkBuilder(row.submissionId as number))
                  : undefined
              }
              onKeyDown={
                canOpen
                  ? (event) => {
                      if (event.key === 'Enter' || event.key === ' ') {
                        event.preventDefault();
                        openSubmission(linkBuilder(row.submissionId as number));
                      }
                    }
                  : undefined
              }
              role={canOpen ? 'link' : undefined}
              tabIndex={canOpen ? 0 : undefined}
            >
              <div className="pt-0.5 font-mono text-xs text-muted-foreground tabular-nums">
                {row.submissionId != null ? `#${row.submissionId}` : '#...'}
              </div>

              <div>
                <div className="flex items-center justify-between gap-2">
                  <Badge variant={variant} className="text-xs">
                    {label}
                  </Badge>

                  {scoreDisplay.kind === 'score' ? (
                    <div
                      className={`inline-flex items-end gap-1 font-mono tabular-nums ${scoreToneClass}`}
                    >
                      <span className="text-2xl font-extrabold leading-none tracking-tight">
                        {scoreDisplay.value}
                      </span>
                      <span className="pb-0.5 text-xs font-semibold tracking-wide opacity-85">
                        {t('result.pointsUnit')}
                      </span>
                    </div>
                  ) : (
                    <div className="inline-flex min-w-16 justify-end text-xs font-semibold text-muted-foreground">
                      {scoreDisplay.label}
                    </div>
                  )}
                </div>

                <div className="mt-1 flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
                  <span>
                    {row.createdAt
                      ? format.relative(row.createdAt)
                      : t('result.judging')}
                  </span>
                  {row.timeUsed != null && (
                    <span>
                      {t('result.time', { value: String(row.timeUsed) })}
                    </span>
                  )}
                  {row.memoryUsed != null && (
                    <span>
                      {t('result.memory', {
                        value: formatKibibytes(row.memoryUsed),
                      })}
                    </span>
                  )}
                </div>
              </div>
            </article>
          );
        })}
      </div>
    </section>
  );
}
