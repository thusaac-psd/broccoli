import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge } from '@broccoli/web-sdk/ui';
import { useParams } from 'react-router';

import { useIsIoiContest } from './hooks/useIsIoiContest';
import type { ScoringMode } from './types';

const SCORING_MODE_KEYS: Record<
  ScoringMode,
  { title: string; description: string }
> = {
  max_submission: {
    title: 'ioi.contestInfo.scoringMode.maxSubmission.title',
    description: 'ioi.contestInfo.scoringMode.maxSubmission.description',
  },
  sum_best_subtask: {
    title: 'ioi.contestInfo.scoringMode.sumBestSubtask.title',
    description: 'ioi.contestInfo.scoringMode.sumBestSubtask.description',
  },
  best_tokened_or_last: {
    title: 'ioi.contestInfo.scoringMode.bestTokenedOrLast.title',
    description: 'ioi.contestInfo.scoringMode.bestTokenedOrLast.description',
  },
};

const FEEDBACK_KEYS: Record<string, string> = {
  full: 'ioi.contestInfo.feedback.full',
  subtask_scores: 'ioi.contestInfo.feedback.subtaskScores',
  total_only: 'ioi.contestInfo.feedback.totalOnly',
  none: 'ioi.contestInfo.feedback.none',
};

/** The contest format's proper name, shown as is in every language. */
const FORMAT_NAME = 'IOI';

export function IoiContestInfo() {
  const { contestId } = useParams();
  const cId = contestId ? Number(contestId) : undefined;
  const { isIoi, contestInfo, isLoading } = useIsIoiContest(cId);
  const { t } = useTranslation();

  if (isLoading || !isIoi || !contestInfo) return null;

  const modeKeys = SCORING_MODE_KEYS[contestInfo.scoring_mode as ScoringMode];
  const mode = modeKeys
    ? { title: t(modeKeys.title), description: t(modeKeys.description) }
    : { title: contestInfo.scoring_mode, description: '' };

  return (
    <div className="rounded-lg border border-border bg-card mb-4 p-4 text-left">
      {/* Title row */}
      <div className="flex items-center gap-2 mb-2">
        <Badge
          variant="default"
          className="uppercase text-[11px] font-bold tracking-wide"
        >
          {FORMAT_NAME}
        </Badge>
        <span className="text-sm font-semibold text-foreground">
          {mode.title}
        </span>
      </div>

      {/* Description */}
      <div className="text-xs text-muted-foreground mb-2.5">
        {mode.description}
      </div>

      {/* Metadata row */}
      <div className="flex flex-wrap gap-4 text-xs text-muted-foreground justify-start">
        <MetaItem
          label={
            FEEDBACK_KEYS[contestInfo.feedback_level]
              ? t(FEEDBACK_KEYS[contestInfo.feedback_level])
              : contestInfo.feedback_level
          }
        />
        {contestInfo.token_mode !== 'none' && (
          <MetaItem
            label={
              contestInfo.token_mode === 'fixed_budget'
                ? t('ioi.contestInfo.tokenMode.fixedBudget')
                : t('ioi.contestInfo.tokenMode.regenerating')
            }
          />
        )}
      </div>
    </div>
  );
}

function MetaItem({ label }: { label: string }) {
  return (
    <span className="inline-flex items-center gap-1 rounded bg-muted text-[11px] font-medium">
      {label}
    </span>
  );
}
