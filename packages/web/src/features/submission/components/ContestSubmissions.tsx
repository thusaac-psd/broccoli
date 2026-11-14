import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuth, viewerCacheKey } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  getStatusLabel,
  SUBMISSION_STATUS_FILTER_OPTIONS,
  type SubmissionStatusFilterValue,
  toSubmissionStatus,
} from '@broccoli/web-sdk/submission';
import { Button, FilterDropdown } from '@broccoli/web-sdk/ui';
import { useQuery } from '@tanstack/react-query';
import { BookOpen, Code2, ListFilter, Search } from 'lucide-react';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useSearchParams } from 'react-router';

import { useContestProblems } from '@/features/contest/hooks/use-contest-problems';
import { fetchSupportedLanguages } from '@/features/problem/api/fetch-supported-languages';
import {
  fetchAllPages,
  MAX_PER_PAGE,
} from '@/features/submission/api/fetch-all-pages';
import { fetchContestSubmissions } from '@/features/submission/api/fetch-contest-submissions';
import { useBulkRejudge } from '@/features/submission/hooks/use-bulk-rejudge';
import { useCursorPages } from '@/features/submission/hooks/use-cursor-pages';

import { BulkRejudgeBar, BulkRejudgeToggle } from './BulkRejudge';
import { SubmissionListPager } from './SubmissionListPager';
import { SubmissionsTable } from './SubmissionsTable';

const PER_PAGE = 20;

function parsePositiveInt(value: string | null) {
  if (!value) return null;
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed > 0 ? parsed : null;
}

function parseStatus(value: string | null): SubmissionStatusFilterValue {
  return SUBMISSION_STATUS_FILTER_OPTIONS.includes(
    value as SubmissionStatusFilterValue,
  )
    ? (value as SubmissionStatusFilterValue)
    : 'all';
}

export function ContestSubmissions({ contestId }: { contestId: number }) {
  const { t } = useTranslation();
  const { user } = useAuth();
  const apiClient = useApiClient();
  const [searchParams, setSearchParams] = useSearchParams();
  // Which rows a viewer sees (their own, or everyone's when the contest
  // shows submissions or they are staff) is the server's call; the cache is
  // per viewer because the rows are.
  const viewerKey = viewerCacheKey(user);

  const pages = useCursorPages();
  const appliedProblemId = parsePositiveInt(searchParams.get('problem'));
  const appliedLanguage = searchParams.get('language') ?? null;
  const appliedStatus = parseStatus(searchParams.get('status'));

  const [draftProblemId, setDraftProblemId] = useState<string>(
    appliedProblemId ? String(appliedProblemId) : 'all',
  );
  const [draftLanguage, setDraftLanguage] = useState<string>(
    appliedLanguage ?? 'all',
  );
  const [draftStatus, setDraftStatus] =
    useState<SubmissionStatusFilterValue>(appliedStatus);

  useEffect(() => {
    setDraftProblemId(appliedProblemId ? String(appliedProblemId) : 'all');
    setDraftLanguage(appliedLanguage ?? 'all');
    setDraftStatus(appliedStatus);
  }, [appliedLanguage, appliedProblemId, appliedStatus]);

  const updateSearchParams = useCallback(
    (
      updates: {
        problem?: number | null;
        language?: string | null;
        status?: SubmissionStatusFilterValue;
      },
      options?: { replace?: boolean },
    ) => {
      const next = new URLSearchParams(searchParams);
      const nextProblem =
        updates.problem === undefined ? appliedProblemId : updates.problem;
      const nextLanguage =
        updates.language === undefined ? appliedLanguage : updates.language;
      const nextStatus = updates.status ?? appliedStatus;

      // Any filter change starts the list over.
      next.delete('cursor');

      if (nextProblem) {
        next.set('problem', String(nextProblem));
      } else {
        next.delete('problem');
      }

      if (nextLanguage) {
        next.set('language', nextLanguage);
      } else {
        next.delete('language');
      }

      if (nextStatus !== 'all') {
        next.set('status', nextStatus);
      } else {
        next.delete('status');
      }

      setSearchParams(next, { replace: options?.replace ?? false });
    },
    [
      appliedLanguage,
      appliedProblemId,
      appliedStatus,
      searchParams,
      setSearchParams,
    ],
  );

  const { data: problems = [] } = useContestProblems(contestId);

  const { data: supportedLanguages = [] } = useQuery({
    queryKey: ['supported-languages'],
    queryFn: () => fetchSupportedLanguages(apiClient),
    staleTime: 5 * 60 * 1000,
  });

  const filters = {
    contestId,
    problemId: appliedProblemId,
    language: appliedLanguage,
    status: toSubmissionStatus(appliedStatus),
  };
  const { data: submissionsResult, isLoading } = useQuery({
    queryKey: [
      'contest-submissions-table',
      String(contestId),
      String(appliedProblemId ?? 'all'),
      String(appliedLanguage ?? 'all'),
      appliedStatus,
      pages.cursor,
      viewerKey,
    ],
    enabled: !!user,
    queryFn: () =>
      fetchContestSubmissions(apiClient, {
        ...filters,
        cursor: pages.cursor,
        per_page: PER_PAGE,
        sort_by: 'created_at',
        sort_order: 'desc',
      }),
  });

  const bulk = useBulkRejudge({
    listKey: ['contest-submissions-table', String(contestId)],
    filterKey: ['contest', filters],
    fetchMatching: () =>
      fetchAllPages((cursor) =>
        fetchContestSubmissions(apiClient, {
          ...filters,
          cursor,
          per_page: MAX_PER_PAGE,
          sort_by: 'created_at',
          sort_order: 'desc',
        }),
      ),
  });

  const submissions = submissionsResult?.data ?? [];
  const pagination = submissionsResult?.pagination;

  const problemOptions = useMemo(
    () => [
      { value: 'all', label: t('submissions.filters.allProblems') },
      ...problems.map((p) => ({
        value: String(p.problem_id),
        label: `${p.label}. ${p.problem_title}`,
      })),
    ],
    [problems, t],
  );

  const languageOptions = useMemo(
    () => [
      { value: 'all', label: t('submissions.filters.allLanguages') },
      ...supportedLanguages.map((l) => ({
        value: l.id,
        label: l.name,
      })),
    ],
    [supportedLanguages, t],
  );

  const statusOptions = useMemo(
    () =>
      SUBMISSION_STATUS_FILTER_OPTIONS.map((s) => ({
        value: s,
        label: getStatusLabel(s, t),
      })),
    [t],
  );

  const applyFilters = () => {
    const nextProblemId =
      draftProblemId === 'all' ? null : Number(draftProblemId);
    updateSearchParams({
      problem: nextProblemId,
      language: draftLanguage === 'all' ? null : draftLanguage,
      status: draftStatus,
    });
  };

  return (
    <div className="flex flex-col gap-4">
      {/* Filters */}
      <div className="flex flex-wrap items-center gap-3">
        <FilterDropdown
          icon={<BookOpen className="h-4 w-4" />}
          value={draftProblemId}
          options={problemOptions}
          onChange={setDraftProblemId}
          className="w-[260px]"
        />
        <FilterDropdown
          icon={<Code2 className="h-4 w-4" />}
          value={draftLanguage}
          options={languageOptions}
          onChange={setDraftLanguage}
          className="w-[220px]"
        />
        <FilterDropdown
          icon={<ListFilter className="h-4 w-4" />}
          value={draftStatus}
          options={statusOptions}
          onChange={(next) =>
            setDraftStatus(next as SubmissionStatusFilterValue)
          }
          className="w-[220px]"
        />
        <Button className="h-9 shrink-0" onClick={applyFilters}>
          <Search className="mr-1 h-4 w-4" />
          {t('submissions.filters.search')}
        </Button>
        <BulkRejudgeToggle bulk={bulk} />
      </div>

      <BulkRejudgeBar bulk={bulk} />

      {/* Table */}
      <div className="border rounded-lg overflow-hidden">
        {isLoading ? (
          <div className="flex items-center justify-center py-16">
            <span className="h-5 w-5 rounded-full border-2 border-primary border-t-transparent animate-spin" />
          </div>
        ) : (
          <SubmissionsTable
            submissions={submissions}
            columns={SubmissionsTable.fullColumns}
            {...bulk.tableSelection}
            linkBuilder={(sub) =>
              `/contests/${contestId}/submissions/${sub.id}`
            }
          />
        )}
      </div>

      <SubmissionListPager
        pagination={pagination}
        page={pages.page}
        hasPrevious={pages.hasPrevious}
        onNext={pages.next}
        onPrevious={pages.previous}
      />
    </div>
  );
}
