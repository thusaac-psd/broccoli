import { useTranslation } from '@broccoli/web-sdk/i18n';
import { useParams } from 'react-router';

import { useContestProblems } from '@/features/contest/hooks/use-contest-problems';
import ProblemView from '@/features/problem/components/ProblemView';
import { type RouteHandle, useContestTitle } from '@/lib/route-title';

/** The problem's title and the contest's, from the lists the page loads. */
export const handle: RouteHandle = {
  useTitle: function useContestProblemTitle() {
    const { t } = useTranslation();
    const { problemId, contestId } = useParams();
    const contest = useContestTitle();
    const { data: problems } = useContestProblems(Number(contestId));
    const problem = problems?.find(
      (p) => p.problem_id === Number(problemId),
    )?.problem_title;
    if (!problem) return contest;
    return contest
      ? t('app.pageInContest', { page: problem, contest })
      : problem;
  },
};

export default function ContestProblemDetailPage() {
  const { problemId, contestId } = useParams();
  return (
    <ProblemView problemId={Number(problemId)} contestId={Number(contestId)} />
  );
}
