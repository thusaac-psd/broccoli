import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuth, viewerCacheKey } from '@broccoli/web-sdk/auth';
import { SUBMISSION_VIEW_ALL } from '@broccoli/web-sdk/permissions';
import type { Submission } from '@broccoli/web-sdk/submission';
import { useQuery } from '@tanstack/react-query';
import {
  createContext,
  type MouseEvent,
  use,
  useLayoutEffect,
  useRef,
} from 'react';
import { useLocation, useNavigate, useNavigationType } from 'react-router';

import {
  createSubmissionNavigationState,
  getSubmissionScrollRestoration,
  readSubmissionReturnContext,
  resolveSubmissionReturn,
} from '@/features/submission/utils/navigation';

export const SubmissionNavigationContext = createContext<string | undefined>(
  undefined,
);

export function usePreviousLocationKey() {
  return use(SubmissionNavigationContext);
}

export function useSubmissionNavigation() {
  const location = useLocation();
  const { user } = useAuth();
  const navigate = useNavigate();
  const openSubmission = (to: string) =>
    navigate(to, {
      state: createSubmissionNavigationState(location, user, window.scrollY),
    });

  return {
    openSubmission,
    onSubmissionClick: (event: MouseEvent<HTMLAnchorElement>, to: string) => {
      event.stopPropagation();
      if (
        event.button === 0 &&
        !event.metaKey &&
        !event.ctrlKey &&
        !event.shiftKey &&
        !event.altKey &&
        !event.defaultPrevented
      ) {
        event.preventDefault();
        openSubmission(to);
      }
    },
  };
}

/** React Query content can arrive after the router's initial scroll restore. */
export function useRestoreSubmissionScroll(ready = true) {
  const location = useLocation();
  const action = useNavigationType();
  const top = getSubmissionScrollRestoration(location)?.top;
  const restoredKey = useRef<string | null>(null);
  useLayoutEffect(() => {
    // POP uses the router's latest saved position. Reapplying the old explicit
    // return position would override subsequent scrolling on another visit.
    if (
      action !== 'REPLACE' ||
      !ready ||
      top === undefined ||
      restoredKey.current === location.key
    )
      return;
    const frame = requestAnimationFrame(() => {
      window.scrollTo({ top, behavior: 'instant' });
      restoredKey.current = location.key;
    });
    return () => cancelAnimationFrame(frame);
  }, [action, ready, top, location.key]);
}

export function useSubmissionReturn(
  submission: Submission | null,
  isLoading: boolean,
  routeContestId?: number,
) {
  const location = useLocation();
  const { user, isLoading: isAuthLoading } = useAuth();
  const apiClient = useApiClient();
  const source = readSubmissionReturnContext(location.state, user);
  // Once loaded, the submission itself is authoritative about its contest.
  const contestId = submission ? submission.contest_id : routeContestId;
  const canViewAll = !!user?.permissions.includes(SUBMISSION_VIEW_ALL);
  const checkContest =
    !source &&
    !!user &&
    Number.isSafeInteger(contestId) &&
    (contestId ?? 0) > 0;

  const contestAccess = useQuery({
    queryKey: ['submission-return-contest', user?.id, contestId],
    enabled: checkContest,
    retry: false,
    queryFn: async () => {
      // Checking the list endpoint also covers submission:view_all, which can
      // grant list access independently of access to the contest dashboard.
      const { error } = await apiClient.GET('/contests/{id}/submissions', {
        params: { path: { id: contestId! }, query: { page: 1, per_page: 1 } },
      });
      if (error) throw error;
      return true;
    },
  });

  const canViewContest = checkContest && contestAccess.isSuccess;
  const checkProblem =
    !source &&
    !!user &&
    !isLoading &&
    !canViewAll &&
    (!checkContest || (!contestAccess.isPending && !canViewContest)) &&
    !!submission?.problem_id;
  const problemAccess = useQuery({
    queryKey: [
      'problem',
      submission?.problem_id,
      contestId ?? null,
      viewerCacheKey(user),
    ],
    enabled: checkProblem,
    retry: false,
    queryFn: async ({ signal }) => {
      const { data, error } = await apiClient.GET('/problems/{id}', {
        params: { path: { id: submission!.problem_id } },
        signal,
      });
      if (error) throw error;
      return data;
    },
  });

  if (
    !source &&
    (isAuthLoading ||
      isLoading ||
      (checkContest && contestAccess.isPending) ||
      (checkProblem && problemAccess.isPending))
  ) {
    return null;
  }

  return resolveSubmissionReturn({
    source,
    contestId,
    canViewContest,
    canViewAll,
    problemId: submission?.problem_id,
    canViewProblem: checkProblem && problemAccess.isSuccess,
  });
}
