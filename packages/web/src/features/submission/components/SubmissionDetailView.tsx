import { queryState } from '@broccoli/web-sdk/hooks';
import { useFormat, useTranslation } from '@broccoli/web-sdk/i18n';
import { isTerminalStatus } from '@broccoli/web-sdk/submission';
import { Badge, QueryErrorState } from '@broccoli/web-sdk/ui';
import { formatKibibytes } from '@broccoli/web-sdk/utils';
import { ArrowLeft, Loader2 } from 'lucide-react';
import { Link, useNavigate } from 'react-router';

import { useSubmissionDetail } from '@/features/submission/hooks/use-submission-detail';
import {
  usePreviousLocationKey,
  useSubmissionReturn,
} from '@/features/submission/hooks/use-submission-navigation';
import {
  submissionReturnState,
  type SubmissionReturnTarget,
} from '@/features/submission/utils/navigation';
import { getSubmissionScoreDisplay } from '@/features/submission/utils/score-display';
import { getVerdictBadge } from '@/features/submission/utils/verdict';

import { SubmissionJudgementHistory } from './SubmissionJudgementHistory';
import { SubmissionResult } from './SubmissionResult';

interface SubmissionDetailViewProps {
  submissionId: number;
  /** If provided, links point to contest-scoped routes. Otherwise standalone. */
  contestId?: number;
}

export function SubmissionDetailView({
  submissionId,
  contestId,
}: SubmissionDetailViewProps) {
  const { t } = useTranslation();
  const format = useFormat();
  const detail = queryState(useSubmissionDetail(submissionId));
  const submission = detail.status === 'data' ? detail.data : null;

  const returnTarget = useSubmissionReturn(
    submission,
    detail.status === 'loading',
    contestId,
  );

  if (detail.status === 'loading' || detail.status === 'idle') {
    return (
      <div className="space-y-4">
        <BackLink target={returnTarget} />
        <div className="flex items-center justify-center py-20">
          <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
        </div>
      </div>
    );
  }

  if (detail.status === 'error' || !submission) {
    return (
      <div className="space-y-4">
        <BackLink target={returnTarget} />
        {detail.status === 'error' && (
          <QueryErrorState
            className="py-20"
            error={detail.error}
            retry={detail.retry}
            retrying={detail.retrying}
            notFound={t('submissionDetail.notFound')}
            failure={t('submissionDetail.loadFailed')}
          />
        )}
      </div>
    );
  }

  const status = submission.status;
  const result = submission.result;
  const isTerminal = isTerminalStatus(status);

  const { label: verdictLabel, variant: verdictVariant } = getVerdictBadge(
    result?.verdict ?? null,
    status,
    t,
  );
  const scoreDisplay = getSubmissionScoreDisplay(status, result?.score, t);

  const problemLink = contestId
    ? `/contests/${contestId}/problems/${submission.problem_id}`
    : `/problems/${submission.problem_id}`;

  return (
    <div className="min-w-0 space-y-6">
      <BackLink target={returnTarget} />

      {/* Header card */}
      <div className="rounded-lg border bg-card">
        <div className="px-6 py-5">
          <div className="flex flex-wrap items-center gap-3">
            <h1 className="text-xl font-bold tabular-nums">#{submission.id}</h1>
            <Badge variant={verdictVariant} className="text-sm">
              {!isTerminal && <Loader2 className="mr-1 h-3 w-3 animate-spin" />}
              {verdictLabel}
            </Badge>
            {scoreDisplay.kind === 'score' ? (
              <span className="ml-auto font-mono text-lg font-bold tabular-nums text-foreground">
                {scoreDisplay.value}
                <span className="text-sm font-normal text-muted-foreground ml-1">
                  {t('result.pointsUnit')}
                </span>
              </span>
            ) : (
              <span className="ml-auto text-sm font-semibold text-muted-foreground">
                {scoreDisplay.label}
              </span>
            )}
          </div>

          <div className="mt-4 grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 gap-y-3 gap-x-6 text-sm">
            <MetaItem label={t('submissionDetail.problem')}>
              <Link
                to={problemLink}
                className="font-medium text-foreground hover:text-primary hover:underline transition-colors"
              >
                {submission.problem_title}
              </Link>
            </MetaItem>
            <MetaItem label={t('submissionDetail.user')}>
              <span className="font-medium text-foreground">
                {submission.username}
              </span>
            </MetaItem>
            <MetaItem label={t('submissionDetail.language')}>
              <Badge variant="outline" className="max-w-full font-mono">
                {submission.language}
              </Badge>
            </MetaItem>
            <MetaItem label={t('overview.submitted')}>
              <span className="text-foreground">
                {format.relative(submission.created_at)}
              </span>
            </MetaItem>
            {submission.target_worker_id ? (
              <MetaItem label={t('submissionDetail.pinnedWorker')}>
                <Badge
                  variant="outline"
                  className="max-w-full font-mono text-primary/80"
                  title={t('submissionDetail.pinnedWorkerHint')}
                >
                  {submission.target_worker_id}
                </Badge>
              </MetaItem>
            ) : null}
            {result?.time_used != null && (
              <MetaItem label={t('result.timeHeader')}>
                <span className="font-mono tabular-nums text-foreground">
                  {t('result.timeValue', { value: result.time_used })}
                </span>
              </MetaItem>
            )}
            {result?.memory_used != null && (
              <MetaItem label={t('result.memoryHeader')}>
                <span className="font-mono tabular-nums text-foreground">
                  {formatKibibytes(result.memory_used)}
                </span>
              </MetaItem>
            )}
          </div>
        </div>
      </div>

      {/* Submission content */}
      <div className="rounded-lg border bg-card px-6 py-5">
        {!isTerminal && (
          <div className="flex items-center gap-2 pb-4">
            <Loader2 className="h-4 w-4 animate-spin text-primary" />
            <span className="text-sm text-muted-foreground">
              {status === 'Compiling'
                ? t('result.compiling')
                : status === 'Running'
                  ? t('result.running')
                  : t('result.judging')}
            </span>
          </div>
        )}

        <SubmissionResult submission={submission} />
      </div>

      <SubmissionJudgementHistory submissionId={submission.id} />
    </div>
  );
}

function MetaItem({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="min-w-0 space-y-0.5">
      <div className="text-[11px] font-medium uppercase tracking-wider text-muted-foreground/60">
        {label}
      </div>
      <div className="[overflow-wrap:anywhere]">{children}</div>
    </div>
  );
}

function BackLink({ target }: { target: SubmissionReturnTarget | null }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const previousKey = usePreviousLocationKey();

  if (!target) {
    return (
      <span
        role="status"
        className="inline-flex items-center gap-1.5 text-sm text-muted-foreground"
      >
        <Loader2 className="h-4 w-4 animate-spin" />
        {t('admin.loading')}
      </span>
    );
  }

  return (
    <Link
      to={target.to}
      replace
      state={submissionReturnState(target)}
      onClick={(event) => {
        if (
          target.source &&
          previousKey === target.source.locationKey &&
          event.button === 0 &&
          !event.metaKey &&
          !event.ctrlKey &&
          !event.shiftKey &&
          !event.altKey
        ) {
          event.preventDefault();
          navigate(-1);
        }
      }}
      className="inline-flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground transition-colors"
    >
      <ArrowLeft className="h-4 w-4" />
      {t(target.labelKey)}
    </Link>
  );
}
