import { useApiClient } from '@broccoli/web-sdk/api';
import { isAlreadyDone, useIdempotentRequest } from '@broccoli/web-sdk/hooks';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  type CodeRun,
  isTerminalStatus,
  parseSubmissionError,
  type Submission,
} from '@broccoli/web-sdk/submission';
import { sleep } from '@broccoli/web-sdk/utils';
import { useQueryClient } from '@tanstack/react-query';
import { useCallback, useEffect, useReducer, useRef, useState } from 'react';
import { toast } from 'sonner';

import { pairFanOutSubmissions } from '@/features/submission/hooks/fan-out-response';
import {
  entriesReducer,
  type EntryResult,
  pollEntry,
  type PollResponse,
  pollTarget,
  type SubmissionEntry,
} from '@/features/submission/hooks/submission-entries';
import { toastUnsettled } from '@/lib/idempotency-feedback';

export type { SubmissionEntry } from '@/features/submission/hooks/submission-entries';

const submissionResult = (submission: Submission): EntryResult => ({
  submission,
  terminal: isTerminalStatus(submission.status),
});

const codeRunResult = (codeRun: CodeRun): EntryResult => ({
  codeRun,
  terminal: isTerminalStatus(codeRun.status),
});

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
  const [entries, dispatch] = useReducer(entriesReducer, []);
  const [activeEntryId, setActiveEntryId] = useState<number | null>(null);
  // Idempotency-Key bound to the submission itself: re-clicking Submit after
  // a network error resends the same key (the server replays a submission
  // whose response was lost), while edited code or another language gets a
  // fresh key, so the server can never replay the old code in its place.
  const sendIdempotent = useIdempotentRequest();

  // One poller per entry in `polling`, started and stopped from the entries
  // themselves: an entry that is final, failed or gone has none.
  const pollersRef = useRef(new Map<number, AbortController>());
  useEffect(() => {
    const pollers = pollersRef.current;
    const wanted = new Map(
      entries.flatMap((entry) => {
        const target = pollTarget(entry);
        return target ? [[entry.id, target] as const] : [];
      }),
    );
    for (const [entryId, poller] of pollers) {
      if (!wanted.has(entryId)) {
        poller.abort();
        pollers.delete(entryId);
      }
    }
    for (const [entryId, target] of wanted) {
      if (pollers.has(entryId)) continue;
      const poller = new AbortController();
      pollers.set(entryId, poller);
      const fetchOnce = async (signal: AbortSignal): Promise<PollResponse> => {
        const path = { id: target.id };
        if (target.kind === 'codeRun') {
          const res = await apiClient.GET('/code-runs/{id}', {
            params: { path },
            signal,
          });
          return res.error
            ? { ok: false, status: res.response.status, error: res.error }
            : { ok: true, result: codeRunResult(res.data) };
        }
        const res = await apiClient.GET('/submissions/{id}', {
          params: { path },
          signal,
        });
        return res.error
          ? { ok: false, status: res.response.status, error: res.error }
          : { ok: true, result: submissionResult(res.data) };
      };
      void pollEntry(entryId, poller.signal, {
        fetch: fetchOnce,
        sleep,
        dispatch,
        toError: parseSubmissionError,
      });
    }
  }, [entries, apiClient]);

  useEffect(() => {
    const pollers = pollersRef.current;
    return () => {
      pollers.forEach((poller) => poller.abort());
      pollers.clear();
    };
  }, []);

  const submit = useCallback(
    async (
      files: SubmissionFile[],
      language: string,
      contestType?: string,
      targetWorkerIds?: string[],
    ) => {
      // Lists that show this problem's submissions now include new ones.
      const invalidateSubmissionLists = () => {
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
      };

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

        dispatch({ type: 'added', entries: placeholderEntries });
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

          placeholderEntries.forEach((placeholder, index) => {
            const pairing = pairings[index];
            if (!pairing) return;
            if (pairing.withheld) {
              // The mutation succeeded - `pairing.submission.id` is real -
              // but the caller's own Read visibility into this submission
              // is Deny, so the server disclosed nothing else about it.
              // `GET /submissions/{id}` degrades the same Deny decision to a
              // 404, so the entry says so instead of polling.
              dispatch({
                type: 'failed',
                id: placeholder.id,
                error: {
                  code: 'SUBMISSION_WITHHELD',
                  message: t('toast.submission.fanOutWithheld', {
                    workerId: placeholder.targetWorkerId ?? pairing.workerId,
                  }),
                },
              });
              return;
            }
            dispatch({
              type: 'accepted',
              id: placeholder.id,
              // Safe: `pairing.withheld` is false, so every field this
              // relies on (status, result, ...) is actually present - see
              // `isWithheldFanOutSubmission`.
              result: submissionResult(
                pairing.submission as unknown as Submission,
              ),
            });
          });

          invalidateSubmissionLists();

          toast.success(
            t('toast.submission.fannedOut', {
              count: String(created.length),
            }),
          );
        } catch (err) {
          console.error('Fan-out submission failed:', err);
          const error = parseSubmissionError(err);
          placeholderEntries.forEach((placeholder) =>
            dispatch({ type: 'failed', id: placeholder.id, error }),
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

      dispatch({ type: 'added', entries: [newEntry] });
      setActiveEntryId(entryId);

      try {
        // Null when an earlier send of this very submission was accepted but
        // its response is gone (`isAlreadyDone`).
        let data: Submission | null;

        if (contestId) {
          const body = { files, language };
          const res = await sendIdempotent(
            { contestId, problemId, body },
            (key) =>
              apiClient.POST(
                '/contests/{id}/problems/{problem_id}/submissions',
                {
                  params: {
                    path: { id: contestId, problem_id: problemId },
                  },
                  body,
                  headers: { 'Idempotency-Key': key },
                },
              ),
          );
          if (res.error && !isAlreadyDone(res.error)) throw res.error;
          data = res.data ?? null;
        } else {
          const body = { files, language, contest_type: contestType };
          const res = await sendIdempotent({ problemId, body }, (key) =>
            apiClient.POST('/problems/{id}/submissions', {
              params: { path: { id: problemId } },
              body,
              headers: { 'Idempotency-Key': key },
            }),
          );
          if (res.error && !isAlreadyDone(res.error)) throw res.error;
          data = res.data ?? null;
        }

        invalidateSubmissionLists();
        if (contestId) {
          queryClient.invalidateQueries({
            queryKey: ['contest-submission-languages', contestId],
          });
        }

        if (!data) {
          // It is in the submissions list; there is nothing to poll here.
          dispatch({ type: 'withdrawn', id: entryId });
          setActiveEntryId(null);
          toast.success(t('toast.submission.alreadySubmitted'));
          return;
        }
        dispatch({
          type: 'accepted',
          id: entryId,
          result: submissionResult(data),
        });
        toast.success(t('toast.submission.submitted'));
      } catch (err) {
        if (
          toastUnsettled(err, t, {
            refetch: invalidateSubmissionLists,
            outcomeUnknownKey: 'toast.submission.outcomeUnknown',
          })
        ) {
          // Not a failure of this code: an earlier attempt to submit it may
          // have gone through (the refetched list shows it if so) or is
          // still running. There is nothing to poll here either way.
          dispatch({ type: 'withdrawn', id: entryId });
          setActiveEntryId(null);
          return;
        }
        console.error('Submission failed:', err);
        dispatch({
          type: 'failed',
          id: entryId,
          error: parseSubmissionError(err),
        });
        toast.error(t('toast.submission.error'));
      }
    },
    [apiClient, contestId, problemId, queryClient, sendIdempotent, t],
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

      dispatch({ type: 'added', entries: [newEntry] });
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

        dispatch({
          type: 'accepted',
          id: entryId,
          result: codeRunResult(data),
        });
        toast.success(t('toast.submission.running'));
      } catch (err) {
        console.error('Run failed:', err);
        dispatch({
          type: 'failed',
          id: entryId,
          error: parseSubmissionError(err),
        });
        toast.error(t('toast.submission.error'));
      }
    },
    [apiClient, contestId, problemId, t],
  );

  const submissionEntries = entries.filter((e) => !e.isRun);
  const latestRun = entries.find((e) => e.isRun) ?? null;

  const isAnySubmitting = entries.some(
    (e) => e.status === 'submitting' || e.status === 'polling',
  );

  // A new problem starts with no entries; their pollers stop with them.
  useEffect(() => {
    dispatch({ type: 'reset' });
    setActiveEntryId(null);
  }, [problemId]);

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
