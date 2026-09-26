import { useApiClient } from '@broccoli/web-sdk/api';
import { useIdempotencyKey } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  type CodeRun,
  isTerminalStatus,
  parseSubmissionError,
  type Submission,
  type SubmissionError,
} from '@broccoli/web-sdk/submission';
import { useQueryClient } from '@tanstack/react-query';
import { useCallback, useEffect, useRef, useState } from 'react';
import { toast } from 'sonner';

import { pairFanOutSubmissions } from '@/features/submission/hooks/fan-out-response';

const POLL_INTERVAL_MS = 1000;

export interface SubmissionEntry {
  /** Unique client-side ID for this entry */
  id: number;
  submission: Submission | null;
  codeRun: CodeRun | null;
  status: 'submitting' | 'polling' | 'done' | 'error';
  error: SubmissionError | null;
  /** True for "run code" entries */
  isRun?: boolean;
  /**
   * Shared key linking sibling entries created together by an admin fan-out
   * (one submit, N pinned submissions). Entries with the same groupKey are
   * rendered together as a comparison strip.
   */
  groupKey?: string;
  /**
   * Worker this entry was pinned to, if any. Used by the comparison strip and
   * the per-row label.
   */
  targetWorkerId?: string;
}

interface SubmissionFile {
  filename: string;
  content: string;
}

export interface UseSubmissionsReturn {
  entries: SubmissionEntry[];
  /** Entries filtered to exclude runs (for the submissions panel). */
  submissionEntries: SubmissionEntry[];
  /** The most recent run entry (for inline display in the editor). */
  latestRun: SubmissionEntry | null;
  submit: (
    files: SubmissionFile[],
    language: string,
    contestType?: string,
    /**
     * Admin-only: pin every operation produced by this submit to the named
     * workers. When non-empty, the server creates one pinned submission per
     * worker (fan-out) and we group them under a single client-side group.
     */
    targetWorkerIds?: string[],
  ) => Promise<void>;
  run: (
    files: SubmissionFile[],
    language: string,
    customTestCases: { input: string; expected_output?: string | null }[],
  ) => Promise<void>;
  isAnySubmitting: boolean;
  activeEntryId: number | null;
  setActiveEntryId: (id: number | null) => void;
}

let entryIdCounter = 0;

interface UseSubmissionsOptions {
  problemId: number;
  contestId?: number;
}

export function useSubmissions({
  problemId,
  contestId,
}: UseSubmissionsOptions): UseSubmissionsReturn {
  const { t } = useTranslation();
  const apiClient = useApiClient();
  const queryClient = useQueryClient();
  const [entries, setEntries] = useState<SubmissionEntry[]>([]);
  const [activeEntryId, setActiveEntryId] = useState<number | null>(null);
  const pollersRef = useRef<Map<number, ReturnType<typeof setInterval>>>(
    new Map(),
  );
  // Stable Idempotency-Key per logical submission attempt: retries of the same
  // logical submission (e.g., user re-clicks Submit after a network error)
  // reuse the same key so the backend can return the cached response. The key
  // is reset on success so the next logical submission gets a fresh one.
  const { getKey, resetKey } = useIdempotencyKey();

  const stopPolling = useCallback((entryId: number) => {
    const interval = pollersRef.current.get(entryId);
    if (interval) {
      clearInterval(interval);
      pollersRef.current.delete(entryId);
    }
  }, []);

  const stopAllPolling = useCallback(() => {
    for (const [id, interval] of Array.from(pollersRef.current)) {
      clearInterval(interval);
      pollersRef.current.delete(id);
    }
  }, []);

  const startSubmissionPolling = useCallback(
    (entryId: number, submissionId: number) => {
      stopPolling(entryId);

      const interval = setInterval(async () => {
        try {
          const { data, error: fetchError } = await apiClient.GET(
            '/submissions/{id}',
            { params: { path: { id: submissionId } } },
          );
          if (fetchError) {
            console.error('Failed to poll submission:', fetchError);
            return;
          }

          setEntries((prev) =>
            prev.map((e) => {
              if (e.id !== entryId) return e;
              const isDone = isTerminalStatus(data.status);
              return {
                ...e,
                submission: data,
                status: isDone ? 'done' : 'polling',
              };
            }),
          );

          if (isTerminalStatus(data.status)) {
            stopPolling(entryId);
          }
        } catch (err) {
          console.error('Polling error:', err);
        }
      }, POLL_INTERVAL_MS);

      pollersRef.current.set(entryId, interval);
    },
    [apiClient, stopPolling],
  );

  const startCodeRunPolling = useCallback(
    (entryId: number, codeRunId: number) => {
      stopPolling(entryId);

      const interval = setInterval(async () => {
        try {
          const { data, error: fetchError } = await apiClient.GET(
            '/code-runs/{id}',
            { params: { path: { id: codeRunId } } },
          );
          if (fetchError) {
            console.error('Failed to poll code run:', fetchError);
            return;
          }

          setEntries((prev) =>
            prev.map((e) => {
              if (e.id !== entryId) return e;
              const isDone = isTerminalStatus(data.status);
              return {
                ...e,
                codeRun: data,
                status: isDone ? 'done' : 'polling',
              };
            }),
          );

          if (isTerminalStatus(data.status)) {
            stopPolling(entryId);
          }
        } catch (err) {
          console.error('Code run polling error:', err);
        }
      }, POLL_INTERVAL_MS);

      pollersRef.current.set(entryId, interval);
    },
    [apiClient, stopPolling],
  );

  const submit = useCallback(
    async (
      files: SubmissionFile[],
      language: string,
      contestType?: string,
      targetWorkerIds?: string[],
    ) => {
      // Fan-out path: one pinned submission per worker, grouped together for
      // the comparison-strip UI.
      if (targetWorkerIds && targetWorkerIds.length > 0) {
        const groupKey = `group-${++entryIdCounter}`;
        const placeholderEntries: SubmissionEntry[] = targetWorkerIds.map(
          (workerId) => ({
            id: ++entryIdCounter,
            submission: null,
            codeRun: null,
            status: 'submitting' as const,
            error: null,
            groupKey,
            targetWorkerId: workerId,
          }),
        );

        setEntries((prev) => [...placeholderEntries, ...prev]);
        setActiveEntryId(placeholderEntries[0]?.id ?? null);

        try {
          const res = await apiClient.POST('/admin/submissions/fan-out', {
            body: {
              problem_id: problemId,
              contest_id: contestId ?? null,
              contest_type: contestType ?? null,
              files,
              language,
              target_worker_ids: targetWorkerIds,
            },
          });
          if (res.error) throw res.error;

          const created = res.data.submissions;
          // Pair each created submission back to its placeholder by request
          // order (not by echoing `target_worker_id` back from the
          // response): that field is absent whenever the admin's own Read
          // visibility into a just-created submission is Deny, which a
          // field-equality match can't tell apart from "nothing came back at
          // all". See `fan-out-response.ts` for the full reasoning.
          const pairings = pairFanOutSubmissions(targetWorkerIds, created);

          setEntries((prev) =>
            prev.map((e) => {
              if (e.groupKey !== groupKey) return e;
              const index = placeholderEntries.findIndex((p) => p.id === e.id);
              const pairing = index === -1 ? undefined : pairings[index];
              if (!pairing) return e;

              if (pairing.withheld) {
                // The mutation succeeded - `pairing.submission.id` is real -
                // but the caller's own Read visibility into this submission
                // is Deny, so the server disclosed nothing else about it.
                // Polling `GET /submissions/{id}` would just 404 forever
                // (that endpoint degrades the same Deny decision to a 404
                // rather than a sparse body), so we don't start it, and we
                // say so explicitly rather than leaving the entry looking
                // like it's still in flight.
                return {
                  ...e,
                  status: 'error' as const,
                  error: {
                    code: 'SUBMISSION_WITHHELD',
                    message: t('toast.submission.fanOutWithheld', {
                      workerId: e.targetWorkerId ?? pairing.workerId,
                    }),
                  },
                };
              }

              return {
                ...e,
                // Safe: `pairing.withheld` is false, so every field this
                // relies on (status, result, ...) is actually present - see
                // `isWithheldFanOutSubmission`.
                submission: pairing.submission as unknown as Submission,
                status: 'polling' as const,
              };
            }),
          );

          if (contestId) {
            queryClient.invalidateQueries({
              queryKey: ['contest-submissions-table', String(contestId)],
            });
          }
          queryClient.invalidateQueries({
            queryKey: ['problem-recent-submissions', contestId, problemId],
          });
          queryClient.invalidateQueries({
            queryKey: ['admin-submissions-table'],
          });

          toast.success(
            t('toast.submission.fannedOut', {
              count: String(created.length),
            }),
          );

          pairings.forEach((pairing, index) => {
            if (pairing.withheld) return;
            const placeholder = placeholderEntries[index];
            if (placeholder) {
              startSubmissionPolling(placeholder.id, pairing.submission.id);
            }
          });
        } catch (err) {
          console.error('Fan-out submission failed:', err);
          const submissionError = parseSubmissionError(err);
          setEntries((prev) =>
            prev.map((e) =>
              e.groupKey === groupKey
                ? { ...e, status: 'error' as const, error: submissionError }
                : e,
            ),
          );
          toast.error(t('toast.submission.error'));
        }
        return;
      }

      // Standard single-submission path.
      const entryId = ++entryIdCounter;

      const newEntry: SubmissionEntry = {
        id: entryId,
        submission: null,
        codeRun: null,
        status: 'submitting',
        error: null,
      };

      setEntries((prev) => [newEntry, ...prev]);
      setActiveEntryId(entryId);

      try {
        let data: Submission;
        const idempotencyHeaders = {
          'Idempotency-Key': getKey(),
        };

        if (contestId) {
          const res = await apiClient.POST(
            '/contests/{id}/problems/{problem_id}/submissions',
            {
              params: {
                path: { id: contestId, problem_id: problemId },
              },
              body: { files, language },
              headers: idempotencyHeaders,
            },
          );
          if (res.error) throw res.error;
          data = res.data;
        } else {
          const res = await apiClient.POST('/problems/{id}/submissions', {
            params: { path: { id: problemId } },
            body: { files, language, contest_type: contestType },
            headers: idempotencyHeaders,
          });
          if (res.error) throw res.error;
          data = res.data;
        }

        // Success: clear key so the next logical submission gets a fresh one.
        resetKey();

        setEntries((prev) =>
          prev.map((e) =>
            e.id === entryId
              ? { ...e, submission: data, status: 'polling' as const }
              : e,
          ),
        );

        if (contestId) {
          queryClient.invalidateQueries({
            queryKey: ['contest-submissions-table', String(contestId)],
          });
          queryClient.invalidateQueries({
            queryKey: ['contest-submission-languages', contestId],
          });
        }
        queryClient.invalidateQueries({
          queryKey: ['problem-recent-submissions', contestId, problemId],
        });
        queryClient.invalidateQueries({
          queryKey: ['admin-submissions-table'],
        });

        toast.success(t('toast.submission.submitted'));
        startSubmissionPolling(entryId, data.id);
      } catch (err) {
        // On error: keep the key so a retry of the same logical operation
        // sends the same Idempotency-Key (allowing backend dedup).
        console.error('Submission failed:', err);
        const submissionError = parseSubmissionError(err);
        setEntries((prev) =>
          prev.map((e) =>
            e.id === entryId
              ? { ...e, status: 'error' as const, error: submissionError }
              : e,
          ),
        );
        toast.error(t('toast.submission.error'));
      }
    },
    [
      apiClient,
      contestId,
      getKey,
      problemId,
      queryClient,
      resetKey,
      startSubmissionPolling,
      t,
    ],
  );

  const run = useCallback(
    async (
      files: SubmissionFile[],
      language: string,
      customTestCases: { input: string; expected_output?: string | null }[],
    ) => {
      const entryId = ++entryIdCounter;

      const newEntry: SubmissionEntry = {
        id: entryId,
        submission: null,
        codeRun: null,
        status: 'submitting',
        error: null,
        isRun: true,
      };

      setEntries((prev) => [newEntry, ...prev]);
      setActiveEntryId(entryId);

      const custom_test_cases = customTestCases.map((tc) => ({
        input: tc.input,
        expected_output: tc.expected_output ?? null,
      }));

      try {
        let data: CodeRun;

        if (contestId) {
          const res = await apiClient.POST(
            '/contests/{id}/problems/{problem_id}/code-runs',
            {
              params: {
                path: { id: contestId, problem_id: problemId },
              },
              body: { files, language, custom_test_cases },
            },
          );
          if (res.error) throw res.error;
          data = res.data;
        } else {
          const res = await apiClient.POST('/problems/{id}/code-runs', {
            params: { path: { id: problemId } },
            body: { files, language, custom_test_cases },
          });
          if (res.error) throw res.error;
          data = res.data;
        }

        setEntries((prev) =>
          prev.map((e) =>
            e.id === entryId
              ? { ...e, codeRun: data, status: 'polling' as const }
              : e,
          ),
        );

        toast.success(t('toast.submission.running'));
        startCodeRunPolling(entryId, data.id);
      } catch (err) {
        console.error('Run failed:', err);
        const submissionError = parseSubmissionError(err);
        setEntries((prev) =>
          prev.map((e) =>
            e.id === entryId
              ? { ...e, status: 'error' as const, error: submissionError }
              : e,
          ),
        );
        toast.error(t('toast.submission.error'));
      }
    },
    [apiClient, contestId, problemId, startCodeRunPolling, t],
  );

  const submissionEntries = entries.filter((e) => !e.isRun);
  const latestRun = entries.find((e) => e.isRun) ?? null;

  const isAnySubmitting = entries.some(
    (e) => e.status === 'submitting' || e.status === 'polling',
  );

  // Reset when problem changes
  useEffect(() => {
    stopAllPolling();
    setEntries([]);
    setActiveEntryId(null);
  }, [problemId, stopAllPolling]);

  // Cleanup on unmount
  useEffect(() => {
    return () => stopAllPolling();
  }, [stopAllPolling]);

  return {
    entries,
    submissionEntries,
    latestRun,
    submit,
    run,
    isAnySubmitting,
    activeEntryId,
    setActiveEntryId,
  };
}
