import { getErrorMessage, useApiClient } from '@broccoli/web-sdk/api';
import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  SUBMISSION_REJUDGE,
  SYSTEM_ADMIN,
} from '@broccoli/web-sdk/permissions';
import type { SubmissionJudgement } from '@broccoli/web-sdk/submission';
import { Badge, Button } from '@broccoli/web-sdk/ui';
import { formatRelativeDatetime } from '@broccoli/web-sdk/utils';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  Check,
  ChevronDown,
  ChevronRight,
  Loader2,
  RotateCcw,
  Trash2,
} from 'lucide-react';
import { useMemo, useState } from 'react';
import { toast } from 'sonner';

import { useSystemOverview } from '@/features/system/hooks/useSystemOverview';

import { getVerdictBadge } from '../utils/verdict';
import { TestCaseRow } from './TestCaseRow';

const PRESERVE_TARGET_WORKER = '__preserve__';

interface Props {
  submissionId: number;
}

export function SubmissionJudgementHistory({ submissionId }: Props) {
  const { t } = useTranslation();
  const { user } = useAuth();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const canRejudge = !!user?.permissions.includes(SUBMISSION_REJUDGE);
  // Judgement version history (including pending regrade candidates) is a
  // judging-operations view. Contestants must only ever see the current
  // verdict, so the whole panel is gated behind the rejudge permission.
  const canViewHistory = canRejudge;
  const canPinWorker = !!user?.permissions.includes(SYSTEM_ADMIN);
  const [targetWorkerId, setTargetWorkerId] = useState(PRESERVE_TARGET_WORKER);
  const [expandedJudgementIds, setExpandedJudgementIds] = useState<Set<number>>(
    () => new Set(),
  );

  // Worker list only feeds the admin worker-pin dropdown below, so only fetch
  // (and poll) it for system admins. Without this gate the admin-only overview
  // endpoint 403-loops for every contestant viewing a submission.
  const { data: systemOverview } = useSystemOverview({ enabled: canPinWorker });
  const liveWorkers = useMemo(
    () => (systemOverview?.workers ?? []).filter((w) => !w.stale),
    [systemOverview],
  );

  const { data: judgements = [], isLoading } = useQuery({
    queryKey: ['submission-judgements', submissionId],
    enabled: canViewHistory,
    queryFn: async () => {
      const { data, error } = await apiClient.GET(
        '/submissions/{id}/judgements',
        {
          params: { path: { id: submissionId } },
        },
      );
      if (error) throw error;
      return data;
    },
  });

  const currentJudgement = useMemo(
    () => judgements.find((judgement) => judgement.is_current) ?? null,
    [judgements],
  );

  const toggleExpanded = (judgementId: number) => {
    setExpandedJudgementIds((previous) => {
      const next = new Set(previous);
      if (next.has(judgementId)) {
        next.delete(judgementId);
      } else {
        next.add(judgementId);
      }
      return next;
    });
  };

  const invalidate = async () => {
    await queryClient.invalidateQueries({
      queryKey: ['submission', submissionId],
    });
    await queryClient.invalidateQueries({
      queryKey: ['submission-judgements', submissionId],
    });
  };

  const rejudgeMutation = useMutation({
    mutationFn: async (applyImmediately: boolean) => {
      const body = {
        apply_immediately: applyImmediately,
        ...(canPinWorker && targetWorkerId !== PRESERVE_TARGET_WORKER
          ? { target_worker_id: targetWorkerId }
          : {}),
      };
      const { data, error } = await apiClient.POST(
        '/submissions/{id}/rejudge',
        {
          params: { path: { id: submissionId } },
          body,
        },
      );
      if (error) throw error;
      return data;
    },
    onSuccess: async () => {
      toast.success(t('submissionDetail.rejudgeQueued'));
      await invalidate();
    },
    onError: (error) => {
      toast.error(getErrorMessage(error, t('submissionDetail.rejudgeError')));
    },
  });

  const applyMutation = useMutation({
    mutationFn: async (judgementId: number) => {
      const { data, error } = await apiClient.POST(
        '/submissions/{id}/judgements/{judgement_id}/apply',
        {
          params: { path: { id: submissionId, judgement_id: judgementId } },
        },
      );
      if (error) throw error;
      return data;
    },
    onSuccess: async () => {
      toast.success(t('submissionDetail.judgementApplied'));
      await invalidate();
    },
    onError: (error) => {
      toast.error(getErrorMessage(error, t('submissionDetail.applyError')));
    },
  });

  const discardMutation = useMutation({
    mutationFn: async (judgementId: number) => {
      const { error } = await apiClient.POST(
        '/submissions/{id}/judgements/{judgement_id}/discard',
        {
          params: { path: { id: submissionId, judgement_id: judgementId } },
        },
      );
      if (error) throw error;
    },
    onSuccess: async () => {
      toast.success(t('submissionDetail.judgementDiscarded'));
      await invalidate();
    },
    onError: (error) => {
      toast.error(getErrorMessage(error, t('submissionDetail.discardError')));
    },
  });

  if (!canViewHistory) return null;

  return (
    <div className="rounded-lg border bg-card px-6 py-5">
      <div className="flex flex-wrap items-center gap-3">
        <h2 className="text-sm font-semibold">
          {t('submissionDetail.versions')}
        </h2>
        {isLoading && (
          <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
        )}
        {canRejudge && (
          <>
            {canPinWorker && (
              <select
                value={targetWorkerId}
                onChange={(event) => setTargetWorkerId(event.target.value)}
                className="ml-auto h-8 rounded-md border bg-background px-2 text-xs"
              >
                <option value={PRESERVE_TARGET_WORKER}>
                  {t('submissionDetail.workerPreserveRouting')}
                </option>
                <option value="">
                  {t('submissionDetail.workerSharedPool')}
                </option>
                {liveWorkers.map((worker) => (
                  <option key={worker.id} value={worker.id}>
                    {worker.id}
                  </option>
                ))}
              </select>
            )}
            <Button
              size="sm"
              variant="outline"
              disabled={rejudgeMutation.isPending}
              onClick={() => rejudgeMutation.mutate(false)}
            >
              {rejudgeMutation.isPending ? (
                <Loader2 className="mr-1 h-4 w-4 animate-spin" />
              ) : (
                <RotateCcw className="mr-1 h-4 w-4" />
              )}
              {t('submissionDetail.regradeCandidate')}
            </Button>
            <Button
              size="sm"
              disabled={rejudgeMutation.isPending}
              onClick={() => rejudgeMutation.mutate(true)}
            >
              <RotateCcw className="mr-1 h-4 w-4" />
              {t('submissionDetail.rejudgeNow')}
            </Button>
          </>
        )}
      </div>

      <div className="mt-4 divide-y">
        {judgements.map((judgement) => (
          <JudgementRow
            key={judgement.id}
            judgement={judgement}
            currentJudgement={currentJudgement}
            expanded={expandedJudgementIds.has(judgement.id)}
            canManage={canRejudge}
            applying={applyMutation.isPending}
            discarding={discardMutation.isPending}
            onToggleExpanded={() => toggleExpanded(judgement.id)}
            onApply={() => applyMutation.mutate(judgement.id)}
            onDiscard={() => discardMutation.mutate(judgement.id)}
          />
        ))}
      </div>
    </div>
  );
}

function JudgementRow({
  judgement,
  currentJudgement,
  expanded,
  canManage,
  applying,
  discarding,
  onToggleExpanded,
  onApply,
  onDiscard,
}: {
  judgement: SubmissionJudgement;
  currentJudgement: SubmissionJudgement | null;
  expanded: boolean;
  canManage: boolean;
  applying: boolean;
  discarding: boolean;
  onToggleExpanded: () => void;
  onApply: () => void;
  onDiscard: () => void;
}) {
  const { t } = useTranslation();
  const { label, variant } = getVerdictBadge(
    judgement.verdict ?? null,
    judgement.status,
    t,
  );

  const detailsVisible =
    judgement.compile_output ||
    judgement.error_code ||
    judgement.error_message ||
    judgement.test_case_results.length > 0;
  const caseChanges = countCaseChanges(judgement, currentJudgement);
  const currentResults = new Map(
    (currentJudgement?.test_case_results ?? []).map((testCase, index) => [
      testCaseKey(testCase, index),
      testCase,
    ]),
  );

  return (
    <div className="py-3 text-sm">
      <div className="flex flex-wrap items-center gap-3">
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="h-7 w-7"
          onClick={onToggleExpanded}
          aria-label={t(
            expanded
              ? 'submissionDetail.collapseVersion'
              : 'submissionDetail.expandVersion',
            { version: String(judgement.version) },
          )}
        >
          {expanded ? (
            <ChevronDown className="h-4 w-4" />
          ) : (
            <ChevronRight className="h-4 w-4" />
          )}
        </Button>
        <div className="w-14 font-mono font-semibold tabular-nums">
          v{judgement.version}
        </div>
        <Badge variant={variant}>{label}</Badge>
        {judgement.is_current && (
          <Badge variant="outline">
            {t('submissionDetail.currentVersion')}
          </Badge>
        )}
        {!judgement.is_finalized && (
          <Badge variant="secondary">
            {t('submissionDetail.pendingVersion')}
          </Badge>
        )}
        {judgement.target_worker_id && (
          <Badge variant="outline" className="font-mono">
            {judgement.target_worker_id}
          </Badge>
        )}
        <span className="text-xs text-muted-foreground">
          {formatRelativeDatetime(judgement.created_at, t)}
        </span>
        {judgement.score != null && (
          <span className="ml-auto font-mono tabular-nums">
            {formatNumber(judgement.score)} {t('result.pointsUnit')}
          </span>
        )}
        {canManage && !judgement.is_current && judgement.is_finalized && (
          <Button
            size="sm"
            variant="outline"
            disabled={applying}
            onClick={onApply}
          >
            <Check className="mr-1 h-4 w-4" />
            {t('submissionDetail.applyVersion')}
          </Button>
        )}
        {canManage && !judgement.is_current && judgement.is_finalized && (
          <Button
            size="sm"
            variant="ghost"
            disabled={discarding}
            onClick={onDiscard}
          >
            <Trash2 className="mr-1 h-4 w-4" />
            {t('submissionDetail.discardVersion')}
          </Button>
        )}
      </div>

      {expanded && (
        <div className="ml-10 mt-3 space-y-4 border-l pl-4">
          <JudgementDeltaSummary
            judgement={judgement}
            currentJudgement={currentJudgement}
            caseChanges={caseChanges}
          />

          {(judgement.compile_output ||
            judgement.error_code ||
            judgement.error_message) && (
            <div className="space-y-2">
              {judgement.error_code && (
                <div className="text-xs text-muted-foreground">
                  {t('submissionDetail.errorCode')}: {judgement.error_code}
                </div>
              )}
              {judgement.error_message && (
                <pre className="overflow-x-auto whitespace-pre-wrap rounded-md bg-muted p-3 text-xs">
                  {judgement.error_message}
                </pre>
              )}
              {judgement.compile_output && (
                <div>
                  <div className="mb-1 text-xs font-medium text-muted-foreground">
                    {t('result.compileOutput')}
                  </div>
                  <pre className="overflow-x-auto whitespace-pre-wrap rounded-md bg-muted p-3 text-xs">
                    {judgement.compile_output}
                  </pre>
                </div>
              )}
            </div>
          )}

          {judgement.test_case_results.length > 0 && (
            <div className="space-y-2">
              <div className="text-xs font-medium text-muted-foreground">
                {t('submissionDetail.resultDetails')}
              </div>
              {judgement.test_case_results.map((testCase, index) => (
                <div key={testCase.id} className="space-y-1">
                  <TestCaseDiffNote
                    currentTestCase={currentResults.get(
                      testCaseKey(testCase, index),
                    )}
                    testCase={testCase}
                  />
                  <TestCaseRow
                    testCase={testCase}
                    index={index + 1}
                    status={judgement.status}
                  />
                </div>
              ))}
            </div>
          )}

          {!detailsVisible && (
            <div className="rounded-md border border-dashed p-3 text-xs text-muted-foreground">
              {t('submissionDetail.noVisibleResultDetails')}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function JudgementDeltaSummary({
  judgement,
  currentJudgement,
  caseChanges,
}: {
  judgement: SubmissionJudgement;
  currentJudgement: SubmissionJudgement | null;
  caseChanges: number;
}) {
  const { t } = useTranslation();
  if (!currentJudgement || judgement.id === currentJudgement.id) return null;

  const scoreDelta = formatDelta(judgement.score, currentJudgement.score);
  const timeDelta = formatDelta(
    judgement.time_used,
    currentJudgement.time_used,
  );
  const memoryDelta = formatDelta(
    judgement.memory_used,
    currentJudgement.memory_used,
  );

  if (!scoreDelta && !timeDelta && !memoryDelta && caseChanges === 0) {
    return null;
  }

  return (
    <div className="flex flex-wrap gap-2 text-xs text-muted-foreground">
      {scoreDelta && (
        <Badge variant="outline">
          {t('submissionDetail.scoreDelta', { value: scoreDelta })}
        </Badge>
      )}
      {timeDelta && (
        <Badge variant="outline">
          {t('submissionDetail.timeDelta', { value: timeDelta })}
        </Badge>
      )}
      {memoryDelta && (
        <Badge variant="outline">
          {t('submissionDetail.memoryDelta', { value: memoryDelta })}
        </Badge>
      )}
      {caseChanges > 0 && (
        <Badge variant="outline">
          {t('submissionDetail.caseChanges', { count: String(caseChanges) })}
        </Badge>
      )}
    </div>
  );
}

function TestCaseDiffNote({
  currentTestCase,
  testCase,
}: {
  currentTestCase: SubmissionJudgement['test_case_results'][number] | undefined;
  testCase: SubmissionJudgement['test_case_results'][number];
}) {
  const { t } = useTranslation();
  if (!currentTestCase) {
    return (
      <Badge variant="outline">{t('submissionDetail.newCaseInVersion')}</Badge>
    );
  }

  if (!testCaseChanged(testCase, currentTestCase)) return null;

  return (
    <Badge variant="outline">
      {t('submissionDetail.changedFromCase', {
        verdict: currentTestCase.verdict,
        score: formatNumber(currentTestCase.score),
      })}
    </Badge>
  );
}

function testCaseKey(
  testCase: SubmissionJudgement['test_case_results'][number],
  index: number,
) {
  return testCase.test_case_id ?? index;
}

function countCaseChanges(
  judgement: SubmissionJudgement,
  currentJudgement: SubmissionJudgement | null,
) {
  if (!currentJudgement || judgement.id === currentJudgement.id) return 0;

  const currentResults = new Map(
    currentJudgement.test_case_results.map((testCase, index) => [
      testCaseKey(testCase, index),
      testCase,
    ]),
  );

  return judgement.test_case_results.filter((testCase, index) => {
    const currentTestCase = currentResults.get(testCaseKey(testCase, index));
    return !currentTestCase || testCaseChanged(testCase, currentTestCase);
  }).length;
}

function testCaseChanged(
  testCase: SubmissionJudgement['test_case_results'][number],
  currentTestCase: SubmissionJudgement['test_case_results'][number],
) {
  return (
    testCase.verdict !== currentTestCase.verdict ||
    testCase.score !== currentTestCase.score ||
    testCase.time_used !== currentTestCase.time_used ||
    testCase.memory_used !== currentTestCase.memory_used ||
    testCase.checker_output !== currentTestCase.checker_output
  );
}

function formatDelta(
  value: number | null | undefined,
  current: number | null | undefined,
) {
  if (value == null || current == null) return null;

  const delta = value - current;
  if (delta === 0) return null;

  return `${delta > 0 ? '+' : ''}${formatNumber(delta)}`;
}

function formatNumber(value: number) {
  return Number.isInteger(value) ? String(value) : value.toFixed(2);
}
