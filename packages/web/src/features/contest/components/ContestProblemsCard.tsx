import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { CONTEST_MANAGE } from '@broccoli/web-sdk/permissions';
import { Skeleton } from '@broccoli/web-sdk/ui';
import { ChevronRight } from 'lucide-react';
import { Link } from 'react-router';

import { useContestInfo } from '@/features/contest/hooks/use-contest-info';
import {
  useContestProblems,
  usePassed,
} from '@/features/contest/hooks/use-contest-problems';

export function ContestProblemsCard({ contestId }: { contestId: number }) {
  const { t } = useTranslation();
  const { user } = useAuth();
  const { contest } = useContestInfo(contestId);

  const canManageContest = !!user?.permissions.includes(CONTEST_MANAGE);
  const started = usePassed(
    contest ? new Date(contest.start_time).getTime() : null,
  );
  const contestNotStarted = !!contest && !started;
  const shouldBlockByStartTime = contestNotStarted && !canManageContest;

  const {
    data: problems = [],
    isLoading,
    error,
  } = useContestProblems(contestId, { enabled: !shouldBlockByStartTime });

  return (
    <div>
      <h3 className="text-lg font-semibold mb-3">{t('contests.problems')}</h3>
      {shouldBlockByStartTime ? (
        <div className="text-sm text-muted-foreground">
          {t('contests.problemsAvailableAfterStart')}
        </div>
      ) : isLoading ? (
        <div className="space-y-2">
          <Skeleton className="h-12 w-full" />
          <Skeleton className="h-12 w-full" />
          <Skeleton className="h-12 w-full" />
        </div>
      ) : error ? (
        <div className="text-sm text-destructive">
          {t('contests.loadProblemsError')}
        </div>
      ) : problems.length === 0 ? (
        <div className="text-sm text-muted-foreground">
          {t('problems.empty')}
        </div>
      ) : (
        <div className="rounded-lg border overflow-hidden">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b bg-muted/50">
                <th className="px-4 py-2.5 text-left font-medium text-muted-foreground w-16">
                  {t('problems.label')}
                </th>
                <th className="px-4 py-2.5 text-left font-medium text-muted-foreground">
                  {t('problems.titleColumn')}
                </th>
                <th className="w-10" />
              </tr>
            </thead>
            <tbody className="divide-y">
              {problems.map((p) => (
                <tr
                  key={p.problem_id}
                  className="group transition-colors hover:bg-muted/50"
                >
                  <td className="px-4 py-3">
                    <span className="inline-flex h-7 w-7 items-center justify-center rounded-md bg-primary/10 text-xs font-bold text-primary">
                      {p.label}
                    </span>
                  </td>
                  <td className="px-4 py-3">
                    <Link
                      to={`/contests/${contestId}/problems/${p.problem_id}`}
                      className="font-medium group-hover:text-primary transition-colors"
                    >
                      {p.problem_title}
                    </Link>
                  </td>
                  <td className="px-2 py-3">
                    <ChevronRight className="h-4 w-4 text-muted-foreground/30 group-hover:text-primary transition-colors" />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
