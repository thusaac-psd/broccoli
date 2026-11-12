import { useApiClient } from '@broccoli/web-sdk/api';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  getStatusLabel,
  SUBMISSION_STATUS_FILTER_OPTIONS,
  type SubmissionStatusFilterValue,
  toSubmissionStatus,
} from '@broccoli/web-sdk/submission';
import { FilterDropdown, Input } from '@broccoli/web-sdk/ui';
import { useQuery } from '@tanstack/react-query';
import { Code2, ListFilter, Search } from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useSearchParams } from 'react-router';

import { fetchSupportedLanguages } from '@/features/problem/api/fetch-supported-languages';
import { fetchSubmissions } from '@/features/submission/api/fetch-submissions';
import { useCursorPages } from '@/features/submission/hooks/use-cursor-pages';

import { SubmissionListPager } from './SubmissionListPager';
import { SubmissionsTable } from './SubmissionsTable';

const PER_PAGE = 25;
const SEARCH_DEBOUNCE_MS = 300;

function parseStatus(value: string | null): SubmissionStatusFilterValue {
  return SUBMISSION_STATUS_FILTER_OPTIONS.includes(
    value as SubmissionStatusFilterValue,
  )
    ? (value as SubmissionStatusFilterValue)
    : 'all';
}

export function AllSubmissions() {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const [searchParams, setSearchParams] = useSearchParams();

  const pages = useCursorPages();
  const appliedQ = (searchParams.get('q') ?? '').trim();
  const appliedLanguage = searchParams.get('language') ?? null;
  const appliedStatus = parseStatus(searchParams.get('status'));

  const [searchInput, setSearchInput] = useState(appliedQ);
  const lastUserEditRef = useRef<number>(0);

  // Sync the input back when URL changes externally (e.g. browser back/forward),
  // but not when the user is actively typing.
  useEffect(() => {
    const sinceLastEdit = Date.now() - lastUserEditRef.current;
    if (sinceLastEdit > SEARCH_DEBOUNCE_MS + 50) {
      setSearchInput(appliedQ);
    }
  }, [appliedQ]);

  const updateSearchParams = useCallback(
    (
      updates: {
        q?: string | null;
        language?: string | null;
        status?: SubmissionStatusFilterValue;
      },
      options?: { replace?: boolean },
    ) => {
      const next = new URLSearchParams(searchParams);
      const nextQ = updates.q === undefined ? appliedQ : updates.q;
      const nextLanguage =
        updates.language === undefined ? appliedLanguage : updates.language;
      const nextStatus = updates.status ?? appliedStatus;

      // Any filter change starts the list over.
      next.delete('cursor');

      if (nextQ && nextQ.trim()) next.set('q', nextQ.trim());
      else next.delete('q');

      if (nextLanguage) next.set('language', nextLanguage);
      else next.delete('language');

      if (nextStatus !== 'all') next.set('status', nextStatus);
      else next.delete('status');

      setSearchParams(next, { replace: options?.replace ?? false });
    },
    [appliedLanguage, appliedQ, appliedStatus, searchParams, setSearchParams],
  );

  // Debounce the text input -> URL.
  useEffect(() => {
    const trimmed = searchInput.trim();
    if (trimmed === appliedQ) return;
    const handle = setTimeout(() => {
      updateSearchParams({ q: trimmed || null }, { replace: true });
    }, SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(handle);
  }, [appliedQ, searchInput, updateSearchParams]);

  const { data: supportedLanguages = [] } = useQuery({
    queryKey: ['supported-languages'],
    queryFn: () => fetchSupportedLanguages(apiClient),
    staleTime: 5 * 60 * 1000,
  });

  const { data: submissionsResult, isLoading } = useQuery({
    queryKey: [
      'admin-submissions-table',
      appliedQ,
      String(appliedLanguage ?? 'all'),
      appliedStatus,
      pages.cursor,
    ],
    queryFn: () =>
      fetchSubmissions(apiClient, {
        q: appliedQ || null,
        language: appliedLanguage,
        status: toSubmissionStatus(appliedStatus),
        cursor: pages.cursor,
        per_page: PER_PAGE,
        sort_by: 'created_at',
        sort_order: 'desc',
      }),
  });

  const submissions = submissionsResult?.data ?? [];
  const pagination = submissionsResult?.pagination;

  const languageOptions = useMemo(
    () => [
      { value: 'all', label: t('submissions.filters.allLanguages') },
      ...supportedLanguages.map((l) => ({ value: l.id, label: l.name })),
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

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <div className="relative flex-1 min-w-[260px] max-w-[480px]">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
          <Input
            type="search"
            placeholder={t('submissions.filters.searchPlaceholder')}
            value={searchInput}
            onChange={(e) => {
              lastUserEditRef.current = Date.now();
              setSearchInput(e.target.value);
            }}
            className="pl-8 h-9"
          />
        </div>
        <FilterDropdown
          icon={<Code2 className="h-4 w-4" />}
          value={appliedLanguage ?? 'all'}
          options={languageOptions}
          onChange={(next) =>
            updateSearchParams({
              language: next === 'all' ? null : next,
            })
          }
          className="w-[200px]"
        />
        <FilterDropdown
          icon={<ListFilter className="h-4 w-4" />}
          value={appliedStatus}
          options={statusOptions}
          onChange={(next) =>
            updateSearchParams({
              status: next as SubmissionStatusFilterValue,
            })
          }
          className="w-[200px]"
        />
      </div>

      <div className="border rounded-lg overflow-hidden">
        {isLoading ? (
          <div className="flex items-center justify-center py-16">
            <span className="h-5 w-5 rounded-full border-2 border-primary border-t-transparent animate-spin" />
          </div>
        ) : (
          <SubmissionsTable
            submissions={submissions}
            columns={SubmissionsTable.adminColumns}
            linkBuilder={(sub) =>
              sub.contest_id
                ? `/contests/${sub.contest_id}/submissions/${sub.id}`
                : `/submissions/${sub.id}`
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
