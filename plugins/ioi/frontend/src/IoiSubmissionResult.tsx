import { useTranslation } from '@broccoli/web-sdk/i18n';
import { isStaff } from '@broccoli/web-sdk/permissions';
import { useSlotPermissions } from '@broccoli/web-sdk/slot';
import {
  isTerminalStatus,
  type ShownStatus,
  type Submission,
  TestCaseOutput,
  type TestCaseResult,
  useTestCasePage,
  verdictStyle,
} from '@broccoli/web-sdk/submission';
import { Badge, PaginatedList } from '@broccoli/web-sdk/ui';
import { cn, formatKibibytes } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { AlertCircle, ChevronDown } from 'lucide-react';
import { type ReactNode, useEffect, useRef, useState } from 'react';

import {
  hostShowsResult,
  resolveFeedbackVisibility,
} from './feedback-visibility';
import { useIoiApi } from './hooks/useIoiApi';
import { useIoiContestInfo } from './hooks/useIoiContestInfo';
import { type DisplayTestCaseResult, memberCell } from './subtask-members';
import type {
  SubtaskInfo,
  SubtaskScoreEntry,
  SubtaskScoresResponse,
  TaskConfigResponse,
} from './types';

interface IoiSubmissionResultProps {
  submission?: Submission | null;
  testCases?: TestCaseResult[];
  children?: ReactNode;
}

type DisplaySubtaskResult = {
  subtask: SubtaskInfo;
  score: number;
};

/** Per-test-case output streams, in display order. */
const TEST_CASE_OUTPUT_FIELDS = [
  'checker_output',
  'stdout',
  'stderr',
  'input',
  'expected_output',
] as const;

const METHOD_META: Record<string, { abbrKey: string; color: string }> = {
  group_min: { abbrKey: 'ioi.submission.method.groupMin', color: '#ef4444' },
  sum: { abbrKey: 'ioi.submission.method.sum', color: '#10b981' },
  group_mul: { abbrKey: 'ioi.submission.method.groupMul', color: '#f59e0b' },
};

function scoreColor(score: number, maxScore: number): string {
  if (maxScore <= 0) return '#6b7280';
  const frac = score / maxScore;
  if (frac >= 1) return '#10b981';
  if (frac > 0) return '#f59e0b';
  return '#6b7280';
}

function formatMs(ms: number): string {
  return ms < 1000 ? `${ms}ms` : `${(ms / 1000).toFixed(2)}s`;
}

/**
 * The subtasks a result shows: the ones its judgement was scored with
 * (`subtaskScores`, stored when it was judged), each listing the members of
 * the problem's current subtask of the same name. Until those arrive, the
 * problem's current subtasks, unscored.
 */
function buildSubtaskResults({
  taskSubtasks,
  subtaskScores,
}: {
  taskSubtasks: SubtaskInfo[];
  subtaskScores: SubtaskScoreEntry[] | null | undefined;
}): DisplaySubtaskResult[] {
  if (!subtaskScores) {
    return taskSubtasks.map((subtask) => ({ subtask, score: 0 }));
  }
  return subtaskScores.map((entry) => ({
    subtask: {
      name: entry.name,
      scoring_method: entry.scoring_method,
      max_score: entry.max_score,
      test_cases: taskSubtasks.find((s) => s.name === entry.name)?.test_cases,
    },
    score: entry.score,
  }));
}

function tcHasDetails(tc: TestCaseResult): boolean {
  return !!(
    tc.checker_output ||
    tc.stdout ||
    tc.stderr ||
    tc.input ||
    tc.expected_output
  );
}

function TestCaseDetailPanel({
  tc,
  index,
  submission,
}: {
  tc: TestCaseResult;
  index: number;
  submission: Submission;
}) {
  const style = verdictStyle(tc.verdict, submission.status);
  const Icon = style.icon;
  const { t } = useTranslation();

  return (
    <div
      className={cn('rounded-md border px-3 py-2.5', style.surface, style.edge)}
    >
      <div className="mb-2 flex items-center gap-1.5 border-b border-border pb-2">
        <Icon size={16} className={cn('shrink-0', style.text)} />
        <span className="text-xs font-semibold text-foreground">
          {t('ioi.submission.testCase', { index: index + 1 })}
        </span>
        {tc.score != null && (
          <span
            className="ml-1 font-mono tabular-nums text-[11px] font-semibold"
            style={{ color: tc.score > 0 ? '#10b981' : '#6b7280' }}
          >
            {t('ioi.submission.score', { score: tc.score })}
          </span>
        )}
        <span className="flex-1" />
        {tc.time_used != null && (
          <span className="font-mono tabular-nums text-[11px] text-muted-foreground">
            {formatMs(tc.time_used)}
          </span>
        )}
        {tc.memory_used != null && (
          <span className="font-mono tabular-nums text-[11px] text-muted-foreground">
            {formatKibibytes(tc.memory_used)}
          </span>
        )}
      </div>
      {TEST_CASE_OUTPUT_FIELDS.map((field) => (
        <TestCaseOutput
          key={field}
          submissionId={submission.id}
          judgementId={submission.result?.judgement_id}
          testCase={tc}
          field={field}
          label={t(
            {
              checker_output: 'ioi.submission.detail.checkerOutput',
              stdout: 'ioi.submission.detail.stdout',
              stderr: 'ioi.submission.detail.stderr',
              input: 'ioi.submission.detail.input',
              expected_output: 'ioi.submission.detail.expectedOutput',
            }[field],
          )}
        />
      ))}
    </div>
  );
}

/**
 * One case of a result's case grid: its verdict, score, time and memory, and
 * a toggle for its details when it has any.
 */
function TestCaseTile({
  tc,
  index,
  status,
  selected,
  onToggle,
}: {
  tc: DisplayTestCaseResult;
  index: number;
  status: ShownStatus;
  selected: boolean;
  onToggle: () => void;
}) {
  const { t } = useTranslation();
  const style = verdictStyle(tc.verdict, status);
  const Icon = tc.isMissing ? AlertCircle : style.icon;
  const clickable = tcHasDetails(tc);
  const tcScore = tc.score ?? 0;
  const tcScoreColor =
    tc.verdict === 'Accepted' ? '#10b981' : tcScore > 0 ? '#f59e0b' : '#6b7280';
  return (
    <div
      role={clickable ? 'button' : undefined}
      tabIndex={clickable ? 0 : undefined}
      aria-expanded={clickable ? selected : undefined}
      onClick={clickable ? onToggle : undefined}
      onKeyDown={
        clickable
          ? (e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                onToggle();
              }
            }
          : undefined
      }
      className={cn(
        'flex items-center gap-1.5 rounded-md px-2 py-1 text-xs transition-all duration-150',
        selected ? style.surfaceActive : style.surface,
        clickable
          ? [
              'cursor-pointer hover:opacity-80',
              selected
                ? ['border-[1.5px]', style.edgeActive]
                : ['border-b-[1.5px]', style.edge],
            ]
          : 'cursor-default',
      )}
    >
      <Icon
        size={14}
        className={cn(
          'shrink-0',
          tc.isMissing ? 'text-amber-700 dark:text-amber-300' : style.text,
        )}
        aria-hidden
      />
      <span className="text-[11px] text-muted-foreground">#{index + 1}</span>
      {tc.isMissing && (
        <span className="truncate text-[11px] text-amber-700 dark:text-amber-300">
          {t('ioi.submission.missingTestCase', { label: tc.label ?? '' })}
        </span>
      )}
      {tc.score != null && (
        <span
          className="font-mono tabular-nums text-[10px] font-semibold"
          style={{ color: tcScoreColor }}
        >
          {tc.score}
        </span>
      )}
      <span className="flex-1" />
      {tc.time_used != null && (
        <span className="font-mono tabular-nums text-[10px] text-muted-foreground">
          {formatMs(tc.time_used)}
        </span>
      )}
      {tc.memory_used != null && (
        <span className="font-mono tabular-nums text-[10px] text-muted-foreground">
          {formatKibibytes(tc.memory_used)}
        </span>
      )}
      {clickable && (
        <ChevronDown
          size={10}
          className={cn(
            'shrink-0 opacity-50 transition-transform duration-200',
            style.text,
            selected && 'rotate-180',
          )}
        />
      )}
    </div>
  );
}

function TestCaseResultList({ submission }: { submission: Submission }) {
  const [page, setPage] = useState(1);
  const query = useTestCasePage({
    submissionId: submission.id,
    judgementId: submission.result?.judgement_id,
    page,
    live: submission.status === 'Running',
  });
  const testCases = query.data?.test_case_results ?? [];
  const { t } = useTranslation();
  const [selectedTcIndex, setSelectedTcIndex] = useState<number | null>(null);
  const selectedTc =
    selectedTcIndex != null
      ? testCases[selectedTcIndex - (page - 1) * 20]
      : null;

  return (
    <div className="overflow-hidden rounded-lg border border-border bg-card">
      {query.isLoading && (
        <p className="p-3 text-xs text-muted-foreground">
          {t('result.loadingOutput')}
        </p>
      )}
      {query.isError && (
        <p className="p-3 text-xs text-destructive">
          {t('result.caseLoadError')}
        </p>
      )}
      <PaginatedList
        items={testCases}
        pagination={query.data?.pagination}
        loading={query.isFetching}
        onPageChange={(nextPage) => {
          setPage(nextPage);
          setSelectedTcIndex(null);
        }}
      >
        {(cases, offset) => (
          <div className="grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-0.5 px-2.5 py-2">
            {cases.map((tc, pageIndex) => {
              const i = offset + pageIndex;
              return (
                <TestCaseTile
                  key={tc.id}
                  tc={tc}
                  index={i}
                  status={submission.status}
                  selected={selectedTcIndex === i}
                  onToggle={() =>
                    setSelectedTcIndex(selectedTcIndex === i ? null : i)
                  }
                />
              );
            })}
          </div>
        )}
      </PaginatedList>

      {selectedTc && selectedTcIndex != null && (
        <div className="px-2.5 pb-2.5">
          <TestCaseDetailPanel
            tc={selectedTc}
            index={selectedTcIndex}
            submission={submission}
          />
        </div>
      )}
    </div>
  );
}

function TotalScoreSummary({
  totalScore,
  maxScore,
  tokened,
}: {
  totalScore: number;
  /** The maximum the total was judged against; unknown until it arrives. */
  maxScore: number | undefined;
  tokened?: boolean;
}) {
  const { t } = useTranslation();

  return (
    <div className="flex items-center justify-center rounded-lg border border-border bg-muted px-5 py-4">
      <div className="flex w-full flex-col items-center justify-center text-center">
        <div className="mb-1.5 flex items-center justify-center gap-1.5 text-[10px] font-semibold uppercase tracking-widest text-muted-foreground">
          {t('ioi.submission.totalScore')}
          {tokened && <TokenedBadge />}
        </div>
        <div
          className="flex items-baseline justify-center font-mono tabular-nums text-2xl font-bold"
          style={
            maxScore === undefined
              ? undefined
              : { color: scoreColor(totalScore, maxScore) }
          }
        >
          {totalScore.toFixed(totalScore === Math.floor(totalScore) ? 0 : 2)}
          {maxScore !== undefined && (
            <span className="text-base font-normal text-muted-foreground">
              /{maxScore.toFixed(maxScore === Math.floor(maxScore) ? 0 : 2)}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

function SubtaskCard({
  subtask,
  score,
  submission,
  labelMap,
  feedbackLevel,
  index,
}: {
  subtask: SubtaskInfo;
  score: number;
  submission: Submission;
  labelMap: Record<string, number>;
  feedbackLevel: string;
  index: number;
}) {
  const [page, setPage] = useState(1);
  const labels = subtask.test_cases ?? [];
  const offset = (page - 1) * 20;
  const pageLabels = labels.slice(offset, offset + 20);
  const ids = pageLabels.flatMap((label) => labelMap[label] ?? []);
  const query = useTestCasePage({
    submissionId: submission.id,
    judgementId: submission.result?.judgement_id,
    testCaseIds: ids,
    enabled: feedbackLevel === 'full',
    live: submission.status === 'Running',
  });
  const results = new Map(
    query.data?.test_case_results.map((tc) => [tc.test_case_id, tc]),
  );
  const testCases = pageLabels.map((label, i) =>
    memberCell(
      label,
      labelMap,
      results,
      -((index + 1) * 10000 + offset + i + 1),
    ),
  );
  const [selectedTcIndex, setSelectedTcIndex] = useState<number | null>(null);
  const maxScore = subtask.max_score;
  const frac = maxScore > 0 ? score / maxScore : 0;
  const color = scoreColor(score, maxScore);
  const { t } = useTranslation();
  const methodRaw = METHOD_META[subtask.scoring_method] ?? {
    abbrKey: '?',
    color: '#6b7280',
  };
  const method = {
    abbr: methodRaw.abbrKey.startsWith('ioi.')
      ? t(methodRaw.abbrKey)
      : methodRaw.abbrKey,
    color: methodRaw.color,
  };

  const selectedTc =
    selectedTcIndex != null ? testCases[selectedTcIndex - offset] : null;

  return (
    <div
      className="overflow-hidden rounded-lg border border-border bg-card"
      style={{ borderLeft: `3px solid ${color}` }}
    >
      {/* Header */}
      <div className="flex items-center justify-between gap-2 px-3.5 py-2.5">
        <div className="flex min-w-0 items-center gap-2">
          <div className="min-w-0">
            <div className="text-[13px] font-semibold text-foreground ml-2">
              {/* The default subtask is stored nameless, for each viewer's
                  language to name. */}
              {subtask.name || t('ioi.subtask.allTests')}
            </div>
          </div>
          <span
            className="rounded font-mono tabular-nums px-1.5 py-px text-[9px] font-bold tracking-wide"
            style={{
              background: `${method.color}14`,
              color: method.color,
            }}
          >
            {method.abbr}
          </span>
        </div>
        <span
          className="whitespace-nowrap font-mono tabular-nums text-sm font-bold mr-2"
          style={{ color }}
        >
          {score.toFixed(score === Math.floor(score) ? 0 : 2)}
          <span className="text-xs font-normal text-muted-foreground">
            /{maxScore.toFixed(maxScore === Math.floor(maxScore) ? 0 : 2)}
          </span>
        </span>
      </div>

      {/* Progress bar */}
      <div className="h-[3px] bg-muted">
        <div
          className="h-full rounded-r-sm transition-[width] duration-[400ms] ease-in-out"
          style={{
            width: `${Math.min(frac * 100, 100)}%`,
            background: `linear-gradient(90deg, ${color}cc, ${color})`,
          }}
        />
      </div>

      {/* Test cases (full feedback only) */}
      {feedbackLevel === 'full' && labels.length > 0 && (
        <div className="px-2.5 py-2">
          {query.isError && (
            <p className="p-2 text-xs text-destructive">
              {t('result.caseLoadError')}
            </p>
          )}
          <PaginatedList
            items={testCases}
            pagination={{
              page,
              per_page: 20,
              total: labels.length,
              total_pages: Math.ceil(labels.length / 20),
            }}
            loading={query.isFetching}
            onPageChange={(nextPage) => {
              setPage(nextPage);
              setSelectedTcIndex(null);
            }}
          >
            {(cases, offset) => (
              <div className="grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-0.5">
                {cases.map((tc, pageIndex) => {
                  const i = offset + pageIndex;
                  return (
                    <TestCaseTile
                      key={tc.id}
                      tc={tc}
                      index={i}
                      status={submission.status}
                      selected={selectedTcIndex === i}
                      onToggle={() =>
                        setSelectedTcIndex(selectedTcIndex === i ? null : i)
                      }
                    />
                  );
                })}
              </div>
            )}
          </PaginatedList>

          {/* Expandable detail panel for selected test case */}
          {selectedTc && selectedTcIndex != null && (
            <div className="mt-1.5">
              <TestCaseDetailPanel
                tc={selectedTc}
                index={selectedTcIndex}
                submission={submission}
              />
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function TokenedBadge() {
  const { t } = useTranslation();
  return (
    <Badge
      variant="secondary"
      className="rounded-full bg-blue-500/10 px-2 py-0.5 text-[10px] font-semibold normal-case tracking-normal text-blue-500"
    >
      {t('ioi.submission.tokened')}
    </Badge>
  );
}

function LoadingSkeleton() {
  return (
    <div className="flex flex-col gap-2">
      {[120, 200, 160].map((w, i) => (
        <div
          key={i}
          className="h-3.5 animate-pulse rounded bg-muted"
          style={{ width: w }}
        />
      ))}
    </div>
  );
}

export function IoiSubmissionResult({
  submission,
  testCases,
  children,
}: IoiSubmissionResultProps) {
  const contestId = submission?.contest_id;
  const problemId = submission?.problem_id;
  const api = useIoiApi();
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const slotPermissions = useSlotPermissions();
  const hasPrivilegedSubmissionAccess = isStaff(
    slotPermissions?.permissions ?? [],
  );

  // Invalidate submission-status when a submission reaches terminal status.
  const prevStatusRef = useRef(submission?.status);
  useEffect(() => {
    const prev = prevStatusRef.current;
    const curr = submission?.status;
    prevStatusRef.current = curr;
    if (
      curr &&
      isTerminalStatus(curr) &&
      prev !== curr &&
      contestId &&
      problemId
    ) {
      queryClient.invalidateQueries({
        queryKey: ['ioi-submission-status', contestId, problemId],
      });
    }
  }, [submission?.status, contestId, problemId, queryClient]);

  const taskConfigQuery = useQuery<TaskConfigResponse>({
    queryKey: ['ioi-task-config', contestId, problemId],
    enabled: !!contestId && !!problemId,
    queryFn: () => api.getTaskConfig(contestId!, problemId!),
    staleTime: 5 * 60 * 1000,
    retry: 2,
  });
  const taskConfig = taskConfigQuery.data;

  const contestInfo = useIoiContestInfo(contestId ?? undefined).data;

  const { data: tokenStatus } = useQuery({
    queryKey: ['ioi-token-status', contestId],
    enabled:
      !!contestId &&
      !!taskConfig &&
      resolveFeedbackVisibility({
        taskConfig,
        contestInfo,
        isTokened: false,
        canViewPrivilegedSubmissionFeedback: hasPrivilegedSubmissionAccess,
      }).needsTokenStatus,
    queryFn: () => api.getTokenStatus(contestId!),
    staleTime: 60000,
  });

  const isTokened =
    tokenStatus?.tokened_submission_ids?.includes(submission?.id ?? -1) ??
    false;
  const visibility = taskConfig
    ? resolveFeedbackVisibility({
        taskConfig,
        contestInfo,
        isTokened,
        canViewPrivilegedSubmissionFeedback: hasPrivilegedSubmissionAccess,
      })
    : null;
  // The judged result (its subtasks, and the maximum it was scored against)
  // backs every level that shows a total.
  const feedbackShowsTotal =
    !!visibility && visibility.effectiveFeedback !== 'none';
  // The server answers once the judgement stands; the key carries the
  // submission's status, so the query asks again when it changes.
  const subtaskScoresQuery = useQuery<SubtaskScoresResponse>({
    queryKey: [
      'ioi-subtask-scores',
      contestId,
      submission?.id,
      submission?.judge_epoch,
      submission?.status,
    ],
    enabled: !!contestId && !!submission?.id && feedbackShowsTotal,
    queryFn: () => api.getSubmissionSubtaskScores(contestId!, submission!.id),
    retry: 2,
  });
  const subtaskScoresData = subtaskScoresQuery.data;

  // Mounted only on IOI contests (the slot's `contest_type`); with no
  // submission yet, the host's own view shows.
  if (!submission || hostShowsResult(submission)) return <>{children}</>;

  // Loading state for task config
  if (!taskConfig && taskConfigQuery.isLoading) {
    return <LoadingSkeleton />;
  }

  // Error state for task config
  if (!taskConfig && taskConfigQuery.isError) {
    return (
      <div className="flex flex-col gap-2">
        <div className="rounded-md border border-amber-500/20 bg-amber-500/[0.06] px-3.5 py-2.5 text-xs text-amber-700">
          {t('ioi.submission.configLoadError')}
        </div>
        {submission?.result?.score != null && (
          <div className="p-3 text-center font-mono tabular-nums text-xl font-bold text-foreground">
            {submission.result.score.toFixed(
              submission.result.score === Math.floor(submission.result.score)
                ? 0
                : 2,
            )}
          </div>
        )}
      </div>
    );
  }

  if (!submission.result || !taskConfig || !visibility) return null;

  // Per-test-case detail (verdict/score/etc.) is rendered only under
  // `effectiveFeedback === 'full'`, which no IOI field mask narrows. Every
  // case's verdict is styled by the SDK's `verdictStyle`, which renders a
  // masked (`null`) verdict of a finished judgement as skipped, as the
  // host's own test case rows do.
  const allTestCases = testCases ?? submission.result.test_case_results ?? [];
  const { effectiveFeedback } = visibility;
  const taskSubtasks = taskConfig.subtasks ?? [];
  const subtaskScores = subtaskScoresData?.subtasks;
  const labelMap: Record<string, number> = taskConfig.label_map ?? {};
  const subtaskResults = buildSubtaskResults({
    taskSubtasks,
    subtaskScores,
  });

  // A system error's message is the host's to show, above this slot.
  if (effectiveFeedback === 'none') {
    return (
      <div className="p-5 text-center text-[13px] italic text-muted-foreground">
        {t('ioi.submission.noFeedback')}
      </div>
    );
  }

  // A judged total reads against the maximum its judgement was scored with,
  // and shows no maximum until that arrives: the problem's current one may
  // differ.
  const configMaxScore = subtaskScoresData?.max_score ?? undefined;

  // Feedback: total_only
  if (effectiveFeedback === 'total_only') {
    const totalScore = submission.result.score ?? 0;
    return (
      <div className="flex flex-col gap-2">
        <TotalScoreSummary
          totalScore={totalScore}
          maxScore={configMaxScore}
          tokened={visibility.usesTokenMode && isTokened}
        />
      </div>
    );
  }

  // Feedback: subtask_scores or full
  if (subtaskResults.length === 0) {
    const totalScore = submission.result.score ?? 0;

    if (
      effectiveFeedback === 'full' &&
      (submission.result.test_case_pagination?.total ?? allTestCases.length) > 0
    ) {
      return (
        <div className="flex flex-col gap-2">
          <TotalScoreSummary
            totalScore={totalScore}
            maxScore={configMaxScore}
            tokened={visibility.usesTokenMode && isTokened}
          />
          <TestCaseResultList
            key={`${submission.id}:${submission.result.judgement_id}`}
            submission={submission}
          />
        </div>
      );
    }

    return (
      <div className="flex flex-col gap-2">
        <TotalScoreSummary totalScore={totalScore} maxScore={configMaxScore} />
      </div>
    );
  }

  // The subtasks the judgement was scored with are still on their way: the
  // problem's current ones may differ, so show neither yet.
  if (!subtaskScores && subtaskScoresQuery.isPending) {
    return <LoadingSkeleton />;
  }

  const totalScore = subtaskResults.reduce(
    (sum: number, r: { score: number }) => sum + r.score,
    0,
  );
  const maxPossible = subtaskResults.reduce(
    (sum: number, result) => sum + result.subtask.max_score,
    0,
  );

  return (
    <div className="flex flex-col gap-2">
      {/* Total score summary bar */}
      <div className="flex items-center justify-between rounded-lg border border-border bg-muted px-3.5 py-2">
        <div className="flex items-center gap-2.5 ml-2">
          <span className="flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
            {t('ioi.submission.total')}
            {visibility.usesTokenMode && isTokened && <TokenedBadge />}
          </span>
        </div>
        <span
          className="font-mono tabular-nums text-base font-bold mr-2"
          style={{ color: scoreColor(totalScore, maxPossible) }}
        >
          {totalScore.toFixed(totalScore === Math.floor(totalScore) ? 0 : 2)}
          <span className="text-[13px] font-normal text-muted-foreground">
            /
            {maxPossible.toFixed(
              maxPossible === Math.floor(maxPossible) ? 0 : 2,
            )}
          </span>
        </span>
      </div>

      {/* Subtask cards */}
      <PaginatedList items={subtaskResults} pageSize={5}>
        {(subtasks, offset) =>
          subtasks.map((r, i) => (
            <SubtaskCard
              key={`${submission.id}:${submission.result!.judgement_id}:${offset + i}`}
              subtask={r.subtask}
              score={r.score}
              submission={submission}
              labelMap={labelMap}
              feedbackLevel={effectiveFeedback}
              index={offset + i}
            />
          ))
        }
      </PaginatedList>

      {/* Resource usage footer */}
      {(submission.result.time_used != null ||
        submission.result.memory_used != null) && (
        <div className="flex justify-end gap-3 py-1">
          {submission.result.time_used != null && (
            <span className="font-mono tabular-nums text-[11px] text-muted-foreground">
              {formatMs(submission.result.time_used)}
            </span>
          )}
          {submission.result.memory_used != null && (
            <span className="font-mono tabular-nums text-[11px] text-muted-foreground">
              {formatKibibytes(submission.result.memory_used)}
            </span>
          )}
        </div>
      )}
    </div>
  );
}
