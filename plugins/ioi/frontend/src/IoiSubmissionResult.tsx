import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useSlotPermissions } from '@broccoli/web-sdk/slot';
import type { Submission, TestCaseResult } from '@broccoli/web-sdk/submission';
import { Badge, Button } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  AlertCircle,
  CheckCircle2,
  ChevronDown,
  Clock,
  Loader2,
  MinusCircle,
  XCircle,
} from 'lucide-react';
import { type ReactNode, useEffect, useRef, useState } from 'react';

import { resolveFeedbackVisibility } from './feedback-visibility';
import { useIoiApi } from './hooks/useIoiApi';
import { useIsIoiContest } from './hooks/useIsIoiContest';
import { canViewPrivilegedSubmissionFeedback } from './permissions';
import type {
  ContestInfoResponse,
  MaskedTestCaseResult,
  SubtaskInfo,
  SubtaskScoreEntry,
  SubtaskScoresResponse,
  TaskConfigResponse,
} from './types';
import { normalizeMaskedTestCase } from './types';

interface IoiSubmissionResultProps {
  submission?: Submission | null;
  testCases?: TestCaseResult[];
  children?: ReactNode;
}

type DisplayTestCaseResult = TestCaseResult & {
  isPlaceholder?: boolean;
  label?: string;
};

type DisplaySubtaskResult = {
  subtask: SubtaskInfo;
  score: number;
  testCases: DisplayTestCaseResult[];
};

const METHOD_META: Record<string, { abbrKey: string; color: string }> = {
  group_min: { abbrKey: 'ioi.submission.method.groupMin', color: '#ef4444' },
  sum: { abbrKey: 'ioi.submission.method.sum', color: '#10b981' },
  group_mul: { abbrKey: 'ioi.submission.method.groupMul', color: '#f59e0b' },
};

const VERDICT_META: Record<string, { color: string; bg: string }> = {
  Accepted: { color: '#10b981', bg: 'rgba(16,185,129,0.1)' },
  WrongAnswer: { color: '#ef4444', bg: 'rgba(239,68,68,0.1)' },
  TimeLimitExceeded: { color: '#f59e0b', bg: 'rgba(245,158,11,0.1)' },
  MemoryLimitExceeded: { color: '#f97316', bg: 'rgba(249,115,22,0.1)' },
  RuntimeError: { color: '#a855f7', bg: 'rgba(168,85,247,0.1)' },
  SystemError: { color: '#6b7280', bg: 'rgba(107,114,128,0.1)' },
  Skipped: { color: '#9ca3af', bg: 'rgba(156,163,175,0.08)' },
  Pending: { color: '#94a3b8', bg: 'rgba(148,163,184,0.1)' },
  Running: { color: '#3b82f6', bg: 'rgba(59,130,246,0.1)' },
};

const VERDICT_ICONS = {
  Accepted: CheckCircle2,
  WrongAnswer: XCircle,
  TimeLimitExceeded: Clock,
  MemoryLimitExceeded: Clock,
  RuntimeError: AlertCircle,
  SystemError: AlertCircle,
  Skipped: MinusCircle,
  Pending: Clock,
  Running: Loader2,
} as const;

const DETAIL_PREVIEW_CHARS = 4096;

function VerdictIcon({
  verdict,
  size = 14,
}: {
  verdict: string;
  size?: number;
}) {
  const meta = VERDICT_META[verdict];
  const c = meta?.color ?? '#6b7280';
  const Icon =
    VERDICT_ICONS[verdict as keyof typeof VERDICT_ICONS] ?? AlertCircle;

  return (
    <Icon
      size={size}
      color={c}
      className={cn('shrink-0', verdict === 'Running' && 'animate-spin')}
    />
  );
}

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

function formatKb(kb: number): string {
  const mb = kb / 1024;
  return `${mb.toFixed(mb >= 10 ? 0 : 1)} MB`;
}

function formatCharCount(count: number): string {
  if (count < 1000) {
    return `${count}`;
  }
  if (count < 1_000_000) {
    return `${(count / 1000).toFixed(count >= 10_000 ? 0 : 1)}K`;
  }
  return `${(count / 1_000_000).toFixed(count >= 10_000_000 ? 0 : 1)}M`;
}

function clamp01(value: number): number {
  return Math.max(0, Math.min(1, value));
}

function createPlaceholderTestCase(
  label: string,
  testCaseId: number | undefined,
  placeholderId: number,
  verdict: 'Pending' | 'Running',
): DisplayTestCaseResult {
  return {
    id: placeholderId,
    test_case_id: testCaseId ?? placeholderId,
    score: 0,
    verdict,
    isPlaceholder: true,
    label,
  };
}

function buildStaticTestCaseList({
  labels,
  labelMap,
  tcById,
  subtaskIndex,
}: {
  labels: string[];
  labelMap: Record<string, number>;
  tcById: Map<number, TestCaseResult>;
  subtaskIndex: number;
}): DisplayTestCaseResult[] {
  return labels.map((label, labelIndex) => {
    const resolvedId =
      labelMap[label] ??
      (Number.isNaN(Number(label)) ? undefined : Number(label));
    const actual = resolvedId != null ? tcById.get(resolvedId) : undefined;
    if (actual) {
      return actual;
    }

    return createPlaceholderTestCase(
      label,
      resolvedId,
      -((subtaskIndex + 1) * 10000 + labelIndex + 1),
      'Pending',
    );
  });
}

function getNormalizedTestCaseScore(
  testCase: DisplayTestCaseResult,
  maxScore: number | undefined,
): number | null {
  if (testCase.isPlaceholder) {
    return null;
  }
  if (!maxScore || maxScore <= 0) {
    return testCase.verdict === 'Accepted' ? 1 : 0;
  }
  return clamp01(testCase.score / maxScore);
}

function computeProvisionalSubtaskScore(
  subtask: SubtaskInfo,
  testCases: DisplayTestCaseResult[],
  testCaseMaxScores: Record<string, number>,
): number {
  const labels = subtask.test_cases ?? [];
  if (labels.length === 0) {
    return 0;
  }

  const normalized = labels.map((label, index) =>
    getNormalizedTestCaseScore(testCases[index], testCaseMaxScores[label]),
  );
  const judged = normalized.filter((value): value is number => value != null);

  if (judged.length === 0) {
    return 0;
  }

  switch (subtask.scoring_method) {
    case 'group_min':
      return judged.every((value) => value >= 1) ? subtask.max_score : 0;
    case 'group_mul':
      return Number(
        (
          judged.reduce((product, value) => product * value, 1) *
          subtask.max_score
        ).toFixed(2),
      );
    case 'sum':
    default:
      return Number(
        (
          (normalized.reduce<number>((sum, value) => sum + (value ?? 0), 0) /
            labels.length) *
          subtask.max_score
        ).toFixed(2),
      );
  }
}

function buildSubtaskResults({
  taskSubtasks,
  subtaskScores,
  effectiveFeedback,
  labelMap,
  testCaseMaxScores,
  allTestCases,
}: {
  taskSubtasks: SubtaskInfo[];
  subtaskScores: SubtaskScoreEntry[] | null | undefined;
  effectiveFeedback: string;
  labelMap: Record<string, number>;
  testCaseMaxScores: Record<string, number>;
  allTestCases: TestCaseResult[];
}): DisplaySubtaskResult[] {
  const tcById = new Map<number, TestCaseResult>();
  for (const testCase of allTestCases) {
    tcById.set(testCase.test_case_id, testCase);
  }

  const subtaskCount = Math.max(
    taskSubtasks.length,
    subtaskScores?.length ?? 0,
  );
  const results: DisplaySubtaskResult[] = [];

  for (let index = 0; index < subtaskCount; index += 1) {
    const scoreEntry = subtaskScores?.[index];
    const configSubtask = taskSubtasks[index];
    if (!configSubtask && !scoreEntry) {
      continue;
    }

    const subtask: SubtaskInfo = configSubtask ?? {
      name: scoreEntry?.name ?? '',
      scoring_method: scoreEntry?.scoring_method ?? 'sum',
      max_score: scoreEntry?.max_score ?? 0,
    };

    const testCases =
      effectiveFeedback === 'full' && subtask.test_cases?.length
        ? buildStaticTestCaseList({
            labels: subtask.test_cases,
            labelMap,
            tcById,
            subtaskIndex: index,
          })
        : [];

    const score =
      scoreEntry?.score ??
      (testCases.length > 0
        ? computeProvisionalSubtaskScore(subtask, testCases, testCaseMaxScores)
        : 0);

    results.push({
      subtask: {
        name: subtask.name,
        scoring_method: subtask.scoring_method,
        max_score: subtask.max_score,
        test_cases: subtask.test_cases,
      },
      score,
      testCases,
    });
  }

  return results;
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

function DetailBlock({ label, content }: { label: string; content: string }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const isLarge = content.length > DETAIL_PREVIEW_CHARS;
  const visibleContent =
    isLarge && !expanded ? content.slice(0, DETAIL_PREVIEW_CHARS) : content;

  return (
    <div className="mb-2">
      <div className="mb-1 flex items-center gap-2">
        <div className="text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">
          {label}
        </div>
        {isLarge && (
          <span className="font-mono tabular-nums text-[10px] text-muted-foreground">
            {expanded
              ? t('ioi.submission.detail.fullSize', {
                  count: formatCharCount(content.length),
                })
              : t('ioi.submission.detail.previewSize', {
                  preview: formatCharCount(DETAIL_PREVIEW_CHARS),
                  total: formatCharCount(content.length),
                })}
          </span>
        )}
      </div>
      <pre className="m-0 max-h-[200px] overflow-y-auto whitespace-pre-wrap break-words rounded-md border border-border bg-muted px-2.5 py-2 font-mono tabular-nums text-xs leading-[18px] text-foreground">
        {visibleContent}
      </pre>
      {isLarge && (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="mt-1 h-auto px-2 py-1 text-[11px] font-medium text-primary"
          onClick={() => setExpanded((value) => !value)}
        >
          {expanded
            ? t('ioi.submission.detail.hideFull')
            : t('ioi.submission.detail.showFull')}
        </Button>
      )}
    </div>
  );
}

function TestCaseDetailPanel({
  tc,
  index,
}: {
  tc: TestCaseResult;
  index: number;
}) {
  const vm = VERDICT_META[tc.verdict] ?? {
    color: '#6b7280',
    bg: 'rgba(0,0,0,0.04)',
  };
  const { t } = useTranslation();

  return (
    <div
      className="rounded-md px-3 py-2.5"
      style={{
        background: vm.bg,
        border: `1px solid ${vm.color}22`,
      }}
    >
      <div className="mb-2 flex items-center gap-1.5 border-b border-border pb-2">
        <VerdictIcon verdict={tc.verdict} size={16} />
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
            {formatKb(tc.memory_used)}
          </span>
        )}
      </div>
      {tc.checker_output && (
        <DetailBlock
          label={t('ioi.submission.detail.checkerOutput')}
          content={tc.checker_output}
        />
      )}
      {tc.stdout && (
        <DetailBlock
          label={t('ioi.submission.detail.stdout')}
          content={tc.stdout}
        />
      )}
      {tc.stderr && (
        <DetailBlock
          label={t('ioi.submission.detail.stderr')}
          content={tc.stderr}
        />
      )}
      {tc.input && (
        <DetailBlock
          label={t('ioi.submission.detail.input')}
          content={tc.input}
        />
      )}
      {tc.expected_output && (
        <DetailBlock
          label={t('ioi.submission.detail.expectedOutput')}
          content={tc.expected_output}
        />
      )}
    </div>
  );
}

function TestCaseResultList({ testCases }: { testCases: TestCaseResult[] }) {
  const [selectedTcIndex, setSelectedTcIndex] = useState<number | null>(null);
  const [hoveredTcIndex, setHoveredTcIndex] = useState<number | null>(null);
  const selectedTc =
    selectedTcIndex != null ? testCases[selectedTcIndex] : null;

  return (
    <div className="overflow-hidden rounded-lg border border-border bg-card">
      <div className="grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-0.5 px-2.5 py-2">
        {testCases.map((tc, i) => {
          const vm = VERDICT_META[tc.verdict] ?? {
            color: '#6b7280',
            bg: 'rgba(0,0,0,0.04)',
          };
          const clickable = tcHasDetails(tc);
          const isSelected = selectedTcIndex === i;
          const tcScore = tc.score ?? 0;
          const tcScoreColor =
            tc.verdict === 'Accepted'
              ? '#10b981'
              : tcScore > 0
                ? '#f59e0b'
                : '#6b7280';

          return (
            <div
              key={tc.id}
              role={clickable ? 'button' : undefined}
              tabIndex={clickable ? 0 : undefined}
              onClick={
                clickable
                  ? () => setSelectedTcIndex(isSelected ? null : i)
                  : undefined
              }
              onKeyDown={
                clickable
                  ? (e) => {
                      if (e.key === 'Enter' || e.key === ' ') {
                        e.preventDefault();
                        setSelectedTcIndex(isSelected ? null : i);
                      }
                    }
                  : undefined
              }
              onMouseEnter={clickable ? () => setHoveredTcIndex(i) : undefined}
              onMouseLeave={
                clickable ? () => setHoveredTcIndex(null) : undefined
              }
              className={cn(
                'flex items-center gap-1.5 rounded-md px-2 py-1 text-xs transition-all duration-150',
                clickable ? 'cursor-pointer' : 'cursor-default',
              )}
              style={{
                background:
                  isSelected || hoveredTcIndex === i ? `${vm.color}20` : vm.bg,
                outline: isSelected ? `1.5px solid ${vm.color}66` : 'none',
                borderBottom: clickable
                  ? `1.5px solid ${isSelected ? vm.color + '66' : vm.color + '30'}`
                  : 'none',
              }}
            >
              <VerdictIcon verdict={tc.verdict} size={14} />
              <span className="text-[11px] text-muted-foreground">
                #{i + 1}
              </span>
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
                  {formatKb(tc.memory_used)}
                </span>
              )}
              {clickable && (
                <ChevronDown
                  size={10}
                  color={vm.color}
                  className="shrink-0 opacity-50 transition-transform duration-200"
                  style={{
                    transform: isSelected ? 'rotate(180deg)' : 'rotate(0deg)',
                  }}
                />
              )}
            </div>
          );
        })}
      </div>

      {selectedTc && selectedTcIndex != null && (
        <div className="px-2.5 pb-2.5">
          <TestCaseDetailPanel tc={selectedTc} index={selectedTcIndex} />
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
  maxScore: number;
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
          style={{ color: scoreColor(totalScore, maxScore) }}
        >
          {totalScore.toFixed(totalScore === Math.floor(totalScore) ? 0 : 2)}
          <span className="text-base font-normal text-muted-foreground">
            /{maxScore.toFixed(maxScore === Math.floor(maxScore) ? 0 : 2)}
          </span>
        </div>
      </div>
    </div>
  );
}

function SubtaskCard({
  subtask,
  score,
  testCases,
  feedbackLevel,
  index,
}: {
  subtask: SubtaskInfo;
  score: number;
  testCases: TestCaseResult[];
  feedbackLevel: string;
  index: number;
}) {
  const [listExpanded, setListExpanded] = useState(false);
  const [selectedTcIndex, setSelectedTcIndex] = useState<number | null>(null);
  const [hoveredTcIndex, setHoveredTcIndex] = useState<number | null>(null);
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

  const INITIAL_VISIBLE = 6;
  const showExpand = testCases.length > INITIAL_VISIBLE;
  const visibleTCs = listExpanded
    ? testCases
    : testCases.slice(0, INITIAL_VISIBLE);
  const selectedTc =
    selectedTcIndex != null ? testCases[selectedTcIndex] : null;

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
              {subtask.name ||
                t('ioi.submission.subtaskFallback', { index: index + 1 })}
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
      {feedbackLevel === 'full' && testCases.length > 0 && (
        <div className="px-2.5 py-2">
          <div className="grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-0.5">
            {visibleTCs.map((tc, i) => {
              const vm = VERDICT_META[tc.verdict] ?? {
                color: '#6b7280',
                bg: 'rgba(0,0,0,0.04)',
              };
              const clickable = tcHasDetails(tc);
              const isSelected = selectedTcIndex === i;
              const tcScore = tc.score ?? 0;
              const tcScoreColor =
                tc.verdict === 'Accepted'
                  ? '#10b981'
                  : tcScore > 0
                    ? '#f59e0b'
                    : '#6b7280';
              return (
                <div
                  key={tc.id}
                  role={clickable ? 'button' : undefined}
                  tabIndex={clickable ? 0 : undefined}
                  onClick={
                    clickable
                      ? () => setSelectedTcIndex(isSelected ? null : i)
                      : undefined
                  }
                  onKeyDown={
                    clickable
                      ? (e) => {
                          if (e.key === 'Enter' || e.key === ' ') {
                            e.preventDefault();
                            setSelectedTcIndex(isSelected ? null : i);
                          }
                        }
                      : undefined
                  }
                  className={cn(
                    'flex items-center gap-1.5 rounded-md px-2 py-1 text-xs transition-all duration-150',
                    clickable ? 'cursor-pointer' : 'cursor-default',
                  )}
                  style={{
                    background:
                      isSelected || hoveredTcIndex === i
                        ? `${vm.color}20`
                        : vm.bg,
                    outline: isSelected ? `1.5px solid ${vm.color}66` : 'none',
                    borderBottom: clickable
                      ? `1.5px solid ${isSelected ? vm.color + '66' : vm.color + '30'}`
                      : 'none',
                  }}
                  onMouseEnter={
                    clickable ? () => setHoveredTcIndex(i) : undefined
                  }
                  onMouseLeave={
                    clickable ? () => setHoveredTcIndex(null) : undefined
                  }
                >
                  <VerdictIcon verdict={tc.verdict} size={14} />
                  <span className="text-[11px] text-muted-foreground">
                    #{i + 1}
                  </span>
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
                      {formatKb(tc.memory_used)}
                    </span>
                  )}
                  {clickable && (
                    <ChevronDown
                      size={10}
                      color={vm.color}
                      className="shrink-0 opacity-50 transition-transform duration-200"
                      style={{
                        transform: isSelected
                          ? 'rotate(180deg)'
                          : 'rotate(0deg)',
                      }}
                    />
                  )}
                </div>
              );
            })}
          </div>

          {/* Expandable detail panel for selected test case */}
          {selectedTc && selectedTcIndex != null && (
            <div className="mt-1.5">
              <TestCaseDetailPanel tc={selectedTc} index={selectedTcIndex} />
            </div>
          )}

          {showExpand && (
            <Button
              variant="ghost"
              size="sm"
              className="mt-1 h-auto px-2.5 py-1 text-[11px] font-medium text-primary"
              onClick={() => {
                if (
                  listExpanded &&
                  selectedTcIndex != null &&
                  selectedTcIndex >= INITIAL_VISIBLE
                ) {
                  setSelectedTcIndex(null);
                }
                setListExpanded(!listExpanded);
              }}
            >
              {listExpanded
                ? t('ioi.submission.showLess')
                : t('ioi.submission.showAll', { count: testCases.length })}
            </Button>
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

const TERMINAL_STATUSES = new Set([
  'Judged',
  'CompilationError',
  'SystemError',
]);

export function IoiSubmissionResult({
  submission,
  testCases,
  children,
}: IoiSubmissionResultProps) {
  const contestId = submission?.contest_id;
  const problemId = submission?.problem_id;
  const { isIoi, isLoading: guardLoading } = useIsIoiContest(
    contestId ?? undefined,
  );
  const api = useIoiApi();
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const slotPermissions = useSlotPermissions();
  const hasPrivilegedSubmissionAccess = canViewPrivilegedSubmissionFeedback(
    slotPermissions?.permissions,
  );

  // Invalidate submission-status when a submission reaches terminal status.
  const prevStatusRef = useRef(submission?.status);
  useEffect(() => {
    const prev = prevStatusRef.current;
    const curr = submission?.status;
    prevStatusRef.current = curr;
    if (
      curr &&
      TERMINAL_STATUSES.has(curr) &&
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
    enabled: !!contestId && !!problemId && isIoi,
    queryFn: () => api.getTaskConfig(contestId!, problemId!),
    staleTime: 5 * 60 * 1000,
    retry: 2,
  });
  const taskConfig = taskConfigQuery.data;

  const contestInfoQuery = useQuery<ContestInfoResponse>({
    queryKey: ['ioi-contest-info', contestId],
    enabled: !!contestId && isIoi,
    queryFn: () => api.getContestInfo(contestId!),
    staleTime: 5 * 60 * 1000,
    retry: 2,
  });
  const contestInfo = contestInfoQuery.data;

  const { data: tokenStatus } = useQuery({
    queryKey: ['ioi-token-status', contestId],
    enabled:
      !!contestId &&
      isIoi &&
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
  const feedbackNeedsSubtasks =
    visibility?.effectiveFeedback === 'subtask_scores' ||
    visibility?.effectiveFeedback === 'full';
  const subtaskScoresQuery = useQuery<SubtaskScoresResponse>({
    queryKey: ['ioi-subtask-scores', contestId, submission?.id],
    enabled: !!contestId && !!submission?.id && isIoi && feedbackNeedsSubtasks,
    queryFn: () => api.getSubmissionSubtaskScores(contestId!, submission!.id),
    retry: 2,
  });
  const subtaskScoresData = subtaskScoresQuery.data;

  // Not IOI or no submission - fall through to default slot children
  if (guardLoading || !isIoi) return <>{children}</>;
  if (!submission) return <>{children}</>;

  // Host handles code viewer + compile output above the slot for these states
  if (submission.status === 'CompilationError') return <>{children}</>;

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

  // A masked test case's `verdict`/`score` arrive as `null` (the visibility
  // kernel can only blank, never author, a field); normalize back to the
  // pre-refactor rendered equivalent ("Skipped" / 0) right at ingestion so
  // every downstream consumer below keeps working with the never-null shape
  // it already expects.
  const allTestCases = (
    testCases ?? submission.result.test_case_results ?? []
  ).map((tc) => normalizeMaskedTestCase(tc as MaskedTestCaseResult));
  const { effectiveFeedback } = visibility;
  const taskSubtasks = taskConfig.subtasks ?? [];
  const subtaskScores = subtaskScoresData?.subtasks;
  const labelMap: Record<string, number> = taskConfig.label_map ?? {};
  const testCaseMaxScores: Record<string, number> =
    taskConfig.test_case_max_scores ?? {};
  const subtaskResults = buildSubtaskResults({
    taskSubtasks,
    subtaskScores,
    effectiveFeedback,
    labelMap,
    testCaseMaxScores,
    allTestCases,
  });

  if (effectiveFeedback === 'none') {
    return (
      <div className="flex flex-col gap-2">
        {submission.status === 'SystemError' &&
          submission.result?.error_message && (
            <div className="rounded-md bg-red-500/[0.06] px-3 py-2 text-xs text-red-600">
              {submission.result.error_message}
            </div>
          )}
        <div className="p-5 text-center text-[13px] italic text-muted-foreground">
          {t('ioi.submission.noFeedback')}
        </div>
      </div>
    );
  }

  // Compute max possible score from task config subtasks (fallback to 100 if not available)
  const configMaxScore =
    taskSubtasks.length > 0
      ? taskSubtasks.reduce((sum, s) => sum + s.max_score, 0)
      : 100;

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

    if (effectiveFeedback === 'full' && allTestCases.length > 0) {
      return (
        <div className="flex flex-col gap-2">
          <TotalScoreSummary
            totalScore={totalScore}
            maxScore={configMaxScore}
            tokened={visibility.usesTokenMode && isTokened}
          />
          <TestCaseResultList testCases={allTestCases} />
        </div>
      );
    }

    return (
      <div className="flex flex-col gap-2">
        <TotalScoreSummary totalScore={totalScore} maxScore={configMaxScore} />
      </div>
    );
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
      {subtaskResults.map(
        (
          r: {
            subtask: SubtaskInfo;
            score: number;
            testCases: TestCaseResult[];
          },
          i: number,
        ) => (
          <SubtaskCard
            key={i}
            subtask={r.subtask}
            score={r.score}
            testCases={r.testCases}
            feedbackLevel={effectiveFeedback}
            index={i}
          />
        ),
      )}

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
              {formatKb(submission.result.memory_used)}
            </span>
          )}
        </div>
      )}
    </div>
  );
}
