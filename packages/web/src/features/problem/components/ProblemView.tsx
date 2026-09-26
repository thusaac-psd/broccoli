import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuth, useAuthReady } from '@broccoli/web-sdk/auth';
import { useRegistries } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { PROBLEM_EDIT, SYSTEM_ADMIN } from '@broccoli/web-sdk/permissions';
import type { SubmissionSummary } from '@broccoli/web-sdk/submission';
import { SubmitGatingProvider } from '@broccoli/web-sdk/submission';
import { useQuery } from '@tanstack/react-query';
import { useCallback, useEffect, useState } from 'react';
import { useSearchParams } from 'react-router';

import type { EditorFile } from '@/components/CodeEditor';
import { useContestProblems } from '@/features/contest/hooks/use-contest-problems';
import { useSubmissions } from '@/features/submission/hooks/use-submissions';

import { ProblemCodingTab } from './ProblemCodingTab';
import { ProblemContentTabs, type ProblemViewTab } from './ProblemContentTabs';
import { ProblemDescriptionTab } from './ProblemDescriptionTab';
import { ProblemEditTab } from './ProblemEditTab';
import { ProblemViewHeader } from './ProblemViewHeader';

const INLINE_SAMPLE_MAX_SIZE = 1024;
const RECENT_SUBMISSION_OVERVIEW_COUNT = 3;
type SampleContentMap = Record<number, { input?: string; output?: string }>;
type CopiedNotice = { text: string; top: number; left: number } | null;
type ProblemRouteTab = ProblemViewTab | 'edit';

interface ProblemViewProps {
  problemId: number;
  contestId?: number;
}

export default function ProblemView({
  problemId,
  contestId,
}: ProblemViewProps) {
  const { t } = useTranslation();
  const { user } = useAuth();
  const authReady = useAuthReady();
  const [searchParams, setSearchParams] = useSearchParams();

  const [isCodeFullscreen, setIsCodeFullscreen] = useState(false);
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const [copiedNotice, setCopiedNotice] = useState<CopiedNotice>(null);
  const [contestType, setContestType] = useState<string | undefined>(undefined);
  const [targetWorkers, setTargetWorkers] = useState<string[]>([]);
  const apiClient = useApiClient();
  const { data: registries } = useRegistries();
  const canPinWorker = !!user?.permissions.includes(SYSTEM_ADMIN);

  const rawTab = searchParams.get('tab');
  const routeTab: ProblemRouteTab =
    rawTab === 'coding' || rawTab === 'edit' || rawTab === 'description'
      ? rawTab
      : 'description';
  const showEditPage = routeTab === 'edit';
  const activeTab: ProblemViewTab =
    routeTab === 'coding' ? 'coding' : 'description';

  const setRouteTab = useCallback(
    (tab: ProblemRouteTab) => {
      const nextParams = new URLSearchParams(searchParams);
      if (tab === 'description') {
        nextParams.delete('tab');
      } else {
        nextParams.set('tab', tab);
      }
      setSearchParams(nextParams);
    },
    [searchParams, setSearchParams],
  );

  useEffect(() => {
    setIsCodeFullscreen(false);
    setCopiedKey(null);
    setCopiedNotice(null);
    setContestType(undefined);
    setTargetWorkers([]);
  }, [problemId]);

  const {
    data: problem,
    isLoading,
    error,
  } = useQuery({
    queryKey: ['problem', problemId],
    enabled: authReady && Number.isFinite(problemId),
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/problems/{id}', {
        params: { path: { id: problemId } },
      });
      if (error) throw error;
      return data;
    },
  });

  const { data: contestProblems = [] } = useContestProblems(contestId);

  const { data: submissionHistory = [] } = useQuery<SubmissionSummary[]>({
    queryKey: ['problem-recent-submissions', contestId, problemId, user?.id],
    enabled: Number.isFinite(problemId) && !!user,
    queryFn: async () => {
      if (!user) return [];

      if (contestId) {
        const { data, error } = await apiClient.GET(
          '/contests/{id}/submissions',
          {
            params: {
              path: { id: contestId },
              query: {
                user_id: user.id,
                problem_id: problemId,
                page: 1,
                per_page: 20,
                sort_by: 'created_at',
                sort_order: 'desc',
              },
            },
          },
        );
        if (error || !data) return [];
        return data.data;
      }

      const { data, error } = await apiClient.GET('/submissions', {
        params: {
          query: {
            user_id: user.id,
            problem_id: problemId,
            page: 1,
            per_page: 20,
            sort_by: 'created_at',
            sort_order: 'desc',
          },
        },
      });
      if (error || !data) return [];
      return data.data;
    },
  });

  const { data: sampleContents = {} as SampleContentMap } =
    useQuery<SampleContentMap>({
      queryKey: [
        'problem-sample-contents',
        problemId,
        problem?.samples.map((sample) => [
          sample.id,
          sample.input_size,
          sample.output_size,
        ]),
      ],
      enabled: Number.isFinite(problemId) && !!problem,
      queryFn: async () => {
        if (!problem) return {} as SampleContentMap;

        const entries = await Promise.all(
          problem.samples.map(async (sample) => {
            const shouldLoadInput = sample.input_size <= INLINE_SAMPLE_MAX_SIZE;
            const shouldLoadOutput =
              sample.output_size <= INLINE_SAMPLE_MAX_SIZE;

            if (!shouldLoadInput && !shouldLoadOutput) {
              return [sample.id, {}] as const;
            }

            const { data, error } = await apiClient.GET(
              '/problems/{id}/test-cases/{tc_id}',
              {
                params: { path: { id: problemId, tc_id: sample.id } },
              },
            );
            if (error || !data) return [sample.id, {}] as const;

            return [
              sample.id,
              {
                input: shouldLoadInput ? data.input : undefined,
                output: shouldLoadOutput ? data.expected_output : undefined,
              },
            ] as const;
          }),
        );

        return Object.fromEntries(entries) as SampleContentMap;
      },
    });

  const submissions = useSubmissions({ problemId, contestId });

  const getSampleCaseData = useCallback(
    async (tcId: number) => {
      const { data, error } = await apiClient.GET(
        '/problems/{id}/test-cases/{tc_id}',
        { params: { path: { id: problemId, tc_id: tcId } } },
      );
      if (error || !data) return null;
      return data;
    },
    [apiClient, problemId],
  );

  const onDownloadSample = useCallback(
    async (tcId: number, sampleIndex: number, type: 'input' | 'output') => {
      const data = await getSampleCaseData(tcId);
      if (!data) return;

      const content = type === 'input' ? data.input : data.expected_output;
      const ext = type === 'input' ? 'in' : 'out';
      const fileName = `sample${sampleIndex}.${ext}`;
      const blob = new Blob([content], { type: 'text/plain;charset=utf-8' });
      const url = URL.createObjectURL(blob);

      const link = document.createElement('a');
      link.href = url;
      link.download = fileName;
      link.click();
      URL.revokeObjectURL(url);
    },
    [getSampleCaseData],
  );

  const onCopySample = useCallback(
    async (
      tcId: number,
      sampleIndex: number,
      type: 'input' | 'output',
      anchorEl: HTMLElement,
      inlineContent?: string,
    ) => {
      let content = inlineContent;
      if (content === undefined) {
        const data = await getSampleCaseData(tcId);
        if (!data) return;
        content = type === 'input' ? data.input : data.expected_output;
      }

      try {
        await navigator.clipboard.writeText(content);
      } catch {
        return;
      }

      const key = `${type}-${tcId}`;
      setCopiedKey(key);
      const ext = type === 'input' ? 'in' : 'out';
      const rect = anchorEl.getBoundingClientRect();
      setCopiedNotice({
        text: t('problem.copied', { file: `sample${sampleIndex}.${ext}` }),
        top: rect.top - 8,
        left: rect.left + rect.width / 2,
      });

      window.setTimeout(() => {
        setCopiedKey((current) => (current === key ? null : current));
      }, 1200);
      window.setTimeout(() => {
        setCopiedNotice((current) =>
          current?.text ===
          t('problem.copied', { file: `sample${sampleIndex}.${ext}` })
            ? null
            : current,
        );
      }, 1500);
    },
    [getSampleCaseData, t],
  );

  // Use the first available registry option as fallback
  // TODO: handle the case when the registry is empty more gracefully
  const fallbackContestType = registries?.contest_types?.[0] ?? '';
  const effectiveContestType =
    contestType ?? problem?.default_contest_type ?? fallbackContestType;

  const handleSubmit = useCallback(
    (files: EditorFile[], language: string) => {
      submissions.submit(
        files.map(({ filename, content }) => ({ filename, content })),
        language,
        effectiveContestType,
        canPinWorker && targetWorkers.length > 0 ? targetWorkers : undefined,
      );
    },
    [submissions, effectiveContestType, canPinWorker, targetWorkers],
  );

  const handleRun = useCallback(
    (
      files: EditorFile[],
      language: string,
      customTestCases: { input: string; expected_output?: string | null }[],
    ) => {
      submissions.run(
        files.map(({ filename, content }) => ({ filename, content })),
        language,
        customTestCases,
      );
    },
    [submissions],
  );

  const storageKey = contestId
    ? `contest-${contestId}-problem-${problemId}`
    : `problem-${problemId}`;

  const contestProblemLabel = contestId
    ? contestProblems.find((item) => item.problem_id === problemId)?.label
    : undefined;
  const headerId = contestProblemLabel ?? (problem ? String(problem.id) : '—');
  const submissionDetailLinkBuilder = useCallback(
    (submissionId: number) =>
      contestId
        ? `/contests/${contestId}/submissions/${submissionId}`
        : `/submissions/${submissionId}`,
    [contestId],
  );

  const latestEntry = submissions.submissionEntries[0] ?? null;
  const latestSubmission = latestEntry?.submission ?? null;
  const isSubmitting = submissions.isAnySubmitting;
  const canEdit = !!user && user.permissions.includes(PROBLEM_EDIT);

  useEffect(() => {
    if (routeTab === 'edit' && !canEdit) {
      setRouteTab('description');
    }
  }, [routeTab, canEdit, setRouteTab]);

  if (!problemId || Number.isNaN(problemId)) {
    return (
      <div className="flex flex-col gap-4 p-6">
        <h1 className="text-2xl font-bold">{t('problem.notFound')}</h1>
      </div>
    );
  }

  if (showEditPage) {
    return (
      <div className="flex flex-col flex-1 min-h-0">
        <ProblemViewHeader
          problem={problem}
          headerId={headerId}
          contestId={contestId}
        />
        <div className="flex-1 min-h-0">
          <ProblemEditTab
            problemId={problemId}
            onBack={() => setRouteTab('description')}
          />
        </div>
      </div>
    );
  }

  return (
    <SubmitGatingProvider>
      <div className="flex flex-col flex-1 min-h-0">
        {copiedNotice && (
          <div
            className="fixed z-50 -translate-x-1/2 -translate-y-full rounded-md border bg-background px-3 py-1.5 text-sm shadow-sm"
            style={{ top: copiedNotice.top, left: copiedNotice.left }}
          >
            {copiedNotice.text || t('problem.copiedSimple')}
          </div>
        )}

        <ProblemViewHeader
          problem={problem}
          headerId={headerId}
          contestId={contestId}
        />

        <ProblemContentTabs
          activeTab={activeTab}
          onTabChange={setRouteTab}
          canEdit={canEdit}
          onEdit={() => setRouteTab('edit')}
          descriptionContent={
            <div className="flex-1 overflow-y-auto p-6">
              <div className="mx-auto max-w-4xl">
                <ProblemDescriptionTab
                  problem={problem}
                  isLoading={isLoading}
                  hasError={!!error}
                  sampleContents={sampleContents}
                  copiedKey={copiedKey}
                  onCopySample={onCopySample}
                  onDownloadSample={onDownloadSample}
                />
              </div>
            </div>
          }
          codingContent={
            <ProblemCodingTab
              isCodeFullscreen={isCodeFullscreen}
              onToggleFullscreen={() => setIsCodeFullscreen(!isCodeFullscreen)}
              onSubmit={handleSubmit}
              onRun={handleRun}
              latestRun={submissions.latestRun}
              storageKey={storageKey}
              contestType={effectiveContestType}
              onContestTypeChange={!contestId ? setContestType : undefined}
              contestTypes={
                !contestId ? (registries?.contest_types ?? []) : undefined
              }
              submissionFormat={problem?.submission_format}
              latestSubmission={latestSubmission}
              submissionHistory={submissionHistory}
              submissions={submissions.submissionEntries}
              isSubmitting={isSubmitting}
              overviewVisibleCount={RECENT_SUBMISSION_OVERVIEW_COUNT}
              submissionDetailLinkBuilder={submissionDetailLinkBuilder}
              contestId={contestId}
              problemId={problemId}
              targetWorkers={canPinWorker ? targetWorkers : null}
              onTargetWorkersChange={
                canPinWorker ? setTargetWorkers : undefined
              }
            />
          }
        />
      </div>
    </SubmitGatingProvider>
  );
}
