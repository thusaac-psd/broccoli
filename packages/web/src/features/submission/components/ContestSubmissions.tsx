import { getErrorMessage, useApiClient } from '@broccoli/web-sdk/api';
import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  CONTEST_MANAGE,
  SUBMISSION_REJUDGE,
  SUBMISSION_VIEW_ALL,
} from '@broccoli/web-sdk/permissions';
import {
  getStatusLabel,
  SUBMISSION_STATUS_FILTER_OPTIONS,
  type SubmissionStatusFilterValue,
  toSubmissionStatus,
} from '@broccoli/web-sdk/submission';
import { Button, FilterDropdown } from '@broccoli/web-sdk/ui';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  BookOpen,
  ChevronLeft,
  ChevronRight,
  Code2,
  ListFilter,
  Loader2,
  RotateCcw,
  Search,
} from 'lucide-react';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useSearchParams } from 'react-router';
import { toast } from 'sonner';

import { useContestProblems } from '@/features/contest/hooks/use-contest-problems';
import { fetchSupportedLanguages } from '@/features/problem/api/fetch-supported-languages';
import {
  fetchAllContestSubmissions,
  fetchContestSubmissions,
} from '@/features/submission/api/fetch-contest-submissions';

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
  const queryClient = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();
  const [isBulkMode, setIsBulkMode] = useState(false);
  const [selectedSubmissionIds, setSelectedSubmissionIds] = useState<
    Set<number>
  >(() => new Set<number>());
  const [bulkApplyImmediately, setBulkApplyImmediately] = useState(true);

  const canBulkRejudge = !!user?.permissions.includes(SUBMISSION_REJUDGE);
  const scopedUserId =
    user?.permissions.includes(SUBMISSION_VIEW_ALL) ||
    user?.permissions.includes(CONTEST_MANAGE)
      ? undefined
      : user?.id;

  const page = parsePositiveInt(searchParams.get('page')) ?? 1;
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
        page?: number;
        problem?: number | null;
        language?: string | null;
        status?: SubmissionStatusFilterValue;
      },
      options?: { replace?: boolean },
    ) => {
      const next = new URLSearchParams(searchParams);
      const nextPage = updates.page ?? page;
      const nextProblem =
        updates.problem === undefined ? appliedProblemId : updates.problem;
      const nextLanguage =
        updates.language === undefined ? appliedLanguage : updates.language;
      const nextStatus = updates.status ?? appliedStatus;

      if (nextPage <= 1) {
        next.delete('page');
      } else {
        next.set('page', String(nextPage));
      }

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
      page,
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

  const { data: submissionsResult, isLoading } = useQuery({
    queryKey: [
      'contest-submissions-table',
      String(contestId),
      String(appliedProblemId ?? 'all'),
      String(appliedLanguage ?? 'all'),
      appliedStatus,
      page,
    ],
    enabled: !!user,
    queryFn: () =>
      fetchContestSubmissions(apiClient, {
        contestId,
        problemId: appliedProblemId,
        language: appliedLanguage,
        status: toSubmissionStatus(appliedStatus),
        userId: scopedUserId,
        page,
        per_page: PER_PAGE,
        sort_by: 'created_at',
        sort_order: 'desc',
      }),
  });

  const { data: allSubmissions = [], isLoading: isAllSubmissionsLoading } =
    useQuery({
      queryKey: [
        'contest-submissions-bulk-source',
        String(contestId),
        String(scopedUserId ?? 'all'),
      ],
      enabled: !!user && canBulkRejudge && isBulkMode,
      queryFn: () =>
        fetchAllContestSubmissions(apiClient, {
          contestId,
          userId: scopedUserId,
        }),
    });

  const bulkRejudgeMutation = useMutation({
    mutationFn: async (submissionIds: number[]) => {
      const { data, error } = await apiClient.POST(
        '/submissions/bulk-rejudge',
        {
          body: {
            submission_ids: submissionIds,
            apply_immediately: bulkApplyImmediately,
          },
        },
      );

      if (!error) {
        return data;
      }

      const message = getErrorMessage(error, '');

      // Compatibility fallback for older backend instances that still expect filter fields.
      if (message.includes('At least one filter field must be provided')) {
        let queued = 0;

        for (const submissionId of submissionIds) {
          const single = await apiClient.POST('/submissions/{id}/rejudge', {
            params: { path: { id: submissionId } },
            // Carry the user's choice through the fallback; an empty body
            // would silently default to apply_immediately=true server-side.
            body: { apply_immediately: bulkApplyImmediately },
          });

          if (!single.error) {
            queued += 1;
          }
        }

        return { queued };
      }

      throw error;
    },
    onSuccess: async (data, submissionIds) => {
      toast.success(
        t('submissions.bulkRejudge.queued', { count: data.queued }),
      );

      setSelectedSubmissionIds((prev) => {
        const next = new Set(prev);
        for (const id of submissionIds) {
          next.delete(id);
        }
        return next;
      });

      await queryClient.invalidateQueries({
        queryKey: ['contest-submissions-table', String(contestId)],
      });
      await queryClient.invalidateQueries({
        queryKey: ['contest-submissions-bulk-source', String(contestId)],
      });
    },
    onError: (error) => {
      toast.error(getErrorMessage(error, t('submissions.bulkRejudge.error')));
    },
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

  const matchedSubmissionIds = useMemo(() => {
    if (!canBulkRejudge || !isBulkMode) {
      return [];
    }

    const targetProblemId =
      draftProblemId === 'all' ? null : Number(draftProblemId);
    const targetLanguage = draftLanguage === 'all' ? null : draftLanguage;
    const targetStatus = draftStatus === 'all' ? null : draftStatus;

    return allSubmissions
      .filter((submission) => {
        if (
          targetProblemId !== null &&
          submission.problem_id !== targetProblemId
        ) {
          return false;
        }

        if (targetLanguage !== null && submission.language !== targetLanguage) {
          return false;
        }

        if (targetStatus !== null && submission.status !== targetStatus) {
          return false;
        }

        return true;
      })
      .map((submission) => submission.id);
  }, [
    allSubmissions,
    canBulkRejudge,
    isBulkMode,
    draftLanguage,
    draftProblemId,
    draftStatus,
  ]);

  useEffect(() => {
    if (!canBulkRejudge) {
      setIsBulkMode(false);
      setSelectedSubmissionIds((prev) =>
        prev.size === 0 ? prev : new Set<number>(),
      );
      return;
    }

    if (!isBulkMode) {
      setSelectedSubmissionIds((prev) =>
        prev.size === 0 ? prev : new Set<number>(),
      );
      return;
    }

    const validIds = new Set(allSubmissions.map((submission) => submission.id));

    setSelectedSubmissionIds((prev) => {
      const next = new Set<number>();
      for (const id of prev) {
        if (validIds.has(id)) {
          next.add(id);
        }
      }
      return next.size === prev.size ? prev : next;
    });
  }, [allSubmissions, canBulkRejudge, isBulkMode]);

  const applyFilters = () => {
    const nextProblemId =
      draftProblemId === 'all' ? null : Number(draftProblemId);
    updateSearchParams({
      page: 1,
      problem: nextProblemId,
      language: draftLanguage === 'all' ? null : draftLanguage,
      status: draftStatus,
    });
  };

  const selectMatched = () => {
    if (matchedSubmissionIds.length === 0) {
      return;
    }

    setSelectedSubmissionIds((prev) => {
      const next = new Set(prev);
      for (const id of matchedSubmissionIds) {
        next.add(id);
      }
      return next;
    });
  };

  const unselectMatched = () => {
    if (matchedSubmissionIds.length === 0) {
      return;
    }

    setSelectedSubmissionIds((prev) => {
      const next = new Set(prev);
      for (const id of matchedSubmissionIds) {
        next.delete(id);
      }
      return next;
    });
  };

  const clearSelection = () => {
    setSelectedSubmissionIds(new Set<number>());
  };

  const toggleBulkMode = () => {
    setIsBulkMode((prev) => !prev);
  };

  const triggerBulkRejudge = () => {
    const ids = Array.from(selectedSubmissionIds);
    if (ids.length === 0) {
      toast.error(t('submissions.bulkRejudge.noSelection'));
      return;
    }

    bulkRejudgeMutation.mutate(ids);
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
        {canBulkRejudge && (
          <Button
            className="h-9 shrink-0"
            variant={isBulkMode ? 'secondary' : 'outline'}
            onClick={toggleBulkMode}
          >
            <RotateCcw className="mr-1 h-4 w-4" />
            {isBulkMode
              ? t('submissions.bulkRejudge.exitMode')
              : t('submissions.bulkRejudge.enterMode')}
          </Button>
        )}
      </div>

      {canBulkRejudge && isBulkMode && (
        <div className="flex flex-wrap items-center gap-2 rounded-md border border-dashed px-3 py-2">
          <Button
            variant="outline"
            className="h-8"
            disabled={
              isAllSubmissionsLoading || matchedSubmissionIds.length === 0
            }
            onClick={selectMatched}
          >
            {t('submissions.selection.selectMatched')}
          </Button>
          <Button
            variant="outline"
            className="h-8"
            disabled={
              isAllSubmissionsLoading || matchedSubmissionIds.length === 0
            }
            onClick={unselectMatched}
          >
            {t('submissions.selection.unselectMatched')}
          </Button>
          <Button
            variant="ghost"
            className="h-8"
            disabled={selectedSubmissionIds.size === 0}
            onClick={clearSelection}
          >
            {t('submissions.selection.clear')}
          </Button>
          <span className="text-xs text-muted-foreground tabular-nums">
            {t('submissions.selection.selectedCount', {
              count: selectedSubmissionIds.size,
            })}{' '}
            ·{' '}
            {t('submissions.selection.matchingCount', {
              count: matchedSubmissionIds.length,
            })}
          </span>
          <label className="inline-flex items-center gap-2 text-xs text-muted-foreground">
            <input
              type="checkbox"
              checked={bulkApplyImmediately}
              onChange={(event) =>
                setBulkApplyImmediately(event.currentTarget.checked)
              }
              className="h-4 w-4 rounded border-muted"
            />
            {t('submissions.bulkRejudge.applyImmediately')}
          </label>
          <Button
            className="h-8 ml-auto"
            disabled={
              bulkRejudgeMutation.isPending || selectedSubmissionIds.size === 0
            }
            onClick={triggerBulkRejudge}
          >
            {bulkRejudgeMutation.isPending && (
              <Loader2 className="mr-1 h-4 w-4 animate-spin" />
            )}
            {!bulkRejudgeMutation.isPending && (
              <RotateCcw className="mr-1 h-4 w-4" />
            )}
            {t('submissions.bulkRejudge.action')}
          </Button>
        </div>
      )}

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
            selectable={canBulkRejudge && isBulkMode}
            selectedSubmissionIds={selectedSubmissionIds}
            onToggleSubmissionSelection={(submissionId, checked) => {
              setSelectedSubmissionIds((prev) => {
                const next = new Set(prev);
                if (checked) {
                  next.add(submissionId);
                } else {
                  next.delete(submissionId);
                }
                return next;
              });
            }}
            onToggleSelectVisible={(submissionIds, checked) => {
              setSelectedSubmissionIds((prev) => {
                const next = new Set(prev);
                for (const id of submissionIds) {
                  if (checked) {
                    next.add(id);
                  } else {
                    next.delete(id);
                  }
                }
                return next;
              });
            }}
            linkBuilder={(sub) =>
              `/contests/${contestId}/submissions/${sub.id}`
            }
          />
        )}
      </div>

      {/* Pagination */}
      {pagination && pagination.total_pages > 1 && (
        <div className="flex items-center justify-between text-sm text-muted-foreground">
          <span className="tabular-nums">
            {(page - 1) * PER_PAGE + 1}–
            {Math.min(page * PER_PAGE, pagination.total)} of {pagination.total}
          </span>
          <div className="flex items-center gap-1">
            <Button
              variant="outline"
              size="icon"
              className="h-8 w-8"
              disabled={page <= 1}
              onClick={() => updateSearchParams({ page: page - 1 })}
            >
              <ChevronLeft className="h-4 w-4" />
            </Button>
            <span className="px-3 text-sm tabular-nums">
              {page} / {pagination.total_pages}
            </span>
            <Button
              variant="outline"
              size="icon"
              className="h-8 w-8"
              disabled={page >= pagination.total_pages}
              onClick={() => updateSearchParams({ page: page + 1 })}
            >
              <ChevronRight className="h-4 w-4" />
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
