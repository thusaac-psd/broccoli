import { getErrorMessage, useApiClient } from '@broccoli/web-sdk/api';
import { useAuth, viewerCacheKey } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { SUBMISSION_REJUDGE } from '@broccoli/web-sdk/permissions';
import type { SubmissionSummary } from '@broccoli/web-sdk/submission';
import {
  type QueryKey,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query';
import { useState } from 'react';
import { toast } from 'sonner';

interface BulkRejudgeOptions {
  /**
   * Identifies the list's current filters: the submissions they match are
   * fetched once per key while bulk mode is on.
   */
  filterKey: QueryKey;
  /** Every submission the list's current filters match, on all pages. */
  fetchMatching: () => Promise<SubmissionSummary[]>;
  /** The list's own queries, refetched once a rejudge is queued. */
  listKey: QueryKey;
}

/**
 * Bulk rejudge over a submission list: a mode the viewer turns on (when they
 * may rejudge), a selection that may span pages and filters, and the request
 * that queues it. Every submission list uses this one owner of the flow,
 * rendered with `BulkRejudgeToggle` and `BulkRejudgeBar` (in
 * `components/BulkRejudge.tsx`) and `tableSelection`.
 */
export function useBulkRejudge({
  filterKey,
  fetchMatching,
  listKey,
}: BulkRejudgeOptions) {
  const { t } = useTranslation();
  const { user } = useAuth();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const allowed = !!user?.permissions.includes(SUBMISSION_REJUDGE);
  const [on, setOn] = useState(false);
  const active = allowed && on;
  const [selected, setSelected] = useState<ReadonlySet<number>>(
    () => new Set(),
  );
  const [applyImmediately, setApplyImmediately] = useState(true);

  const matchingKey = [
    'bulk-rejudge-matching',
    viewerCacheKey(user),
    filterKey,
  ];
  const matching = useQuery({
    queryKey: matchingKey,
    enabled: active,
    queryFn: async () => (await fetchMatching()).map((s) => s.id),
  });
  const matchingIds = matching.data ?? [];

  const rejudge = useMutation({
    mutationFn: async (submissionIds: number[]) => {
      const { data, error } = await apiClient.POST(
        '/submissions/bulk-rejudge',
        {
          body: {
            submission_ids: submissionIds,
            apply_immediately: applyImmediately,
          },
        },
      );
      if (error) throw error;
      return data;
    },
    onSuccess: async (data, submissionIds) => {
      toast.success(
        t('submissions.bulkRejudge.queued', { count: data.queued }),
      );
      setSelected((prev) => without(prev, submissionIds));
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: listKey }),
        queryClient.invalidateQueries({ queryKey: ['bulk-rejudge-matching'] }),
      ]);
    },
    onError: (error) => {
      toast.error(getErrorMessage(error, t('submissions.bulkRejudge.error')));
    },
  });

  const setChecked = (ids: number[], checked: boolean) =>
    setSelected((prev) =>
      checked ? including(prev, ids) : without(prev, ids),
    );

  return {
    allowed,
    active,
    /** Turning the mode off drops the selection. */
    toggle: () => {
      setOn((prev) => !prev);
      setSelected(new Set());
    },
    selected,
    matchingIds,
    matchingLoading: matching.isPending,
    selectMatching: () => setChecked(matchingIds, true),
    unselectMatching: () => setChecked(matchingIds, false),
    clear: () => setSelected(new Set()),
    applyImmediately,
    setApplyImmediately,
    pending: rejudge.isPending,
    submit: () => {
      if (selected.size === 0) {
        toast.error(t('submissions.bulkRejudge.noSelection'));
        return;
      }
      rejudge.mutate([...selected]);
    },
    /** Props that make a `SubmissionsTable` selectable while the mode is on. */
    tableSelection: active
      ? {
          selectable: true,
          selectedSubmissionIds: selected,
          onToggleSubmissionSelection: (id: number, checked: boolean) =>
            setChecked([id], checked),
          onToggleSelectVisible: setChecked,
        }
      : {},
  };
}

export type BulkRejudge = ReturnType<typeof useBulkRejudge>;

function including(set: ReadonlySet<number>, ids: number[]) {
  const next = new Set(set);
  for (const id of ids) next.add(id);
  return next;
}

function without(set: ReadonlySet<number>, ids: number[]) {
  const next = new Set(set);
  for (const id of ids) next.delete(id);
  return next;
}
