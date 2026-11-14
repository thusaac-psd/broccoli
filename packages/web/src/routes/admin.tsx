import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { SUBMISSION_VIEW_ALL } from '@broccoli/web-sdk/permissions';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@broccoli/web-sdk/ui';
import { useQuery } from '@tanstack/react-query';
import { Activity, ArrowRight, Clock, Code2, Home, Trophy } from 'lucide-react';
import { Link } from 'react-router';

import { ListSkeleton } from '@/components/ListSkeleton';
import { PageLayout } from '@/components/PageLayout';
import { getContestStatus } from '@/features/contest/utils/status';
import { SubmissionsTable } from '@/features/submission/components/SubmissionsTable';
import { pageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = pageTitle('overview.title');

export default function OverviewPage() {
  const { t } = useTranslation();
  const { user } = useAuth();
  const apiClient = useApiClient();

  const { data: contests, isLoading: isContestsLoading } = useQuery({
    queryKey: ['overview-contests'],
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/contests', {
        params: {
          query: {
            per_page: 5,
            sort_by: 'start_time',
            sort_order: 'desc',
          },
        },
      });
      if (error) throw error;
      return data.data;
    },
  });

  const { data: problems, isLoading: isProblemsLoading } = useQuery({
    queryKey: ['overview-problems'],
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/problems', {
        params: {
          query: {
            per_page: 5,
            sort_by: 'created_at',
            sort_order: 'desc',
          },
        },
      });
      if (error) throw error;
      return data.data;
    },
  });

  const { data: submissions, isLoading: isSubmissionsLoading } = useQuery({
    queryKey: ['overview-submissions', user?.id],
    enabled: !!user,
    queryFn: async () => {
      const { data, error } = await apiClient.GET('/submissions', {
        params: {
          query: {
            per_page: 5,
            sort_by: 'created_at',
            sort_order: 'desc',
          },
        },
      });
      if (error) throw error;
      return data.data;
    },
  });

  return (
    <PageLayout
      pageId="overview"
      title={t('overview.title')}
      icon={<Home className="h-6 w-6 text-primary" />}
    >
      <div className="grid gap-6 md:grid-cols-2">
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <CardTitle className="flex items-center gap-2">
                <Trophy className="h-4 w-4" />
                {t('overview.contests')}
              </CardTitle>
              <Button variant="ghost" size="sm" asChild>
                <Link to="/contests">
                  {t('overview.viewAll')}
                  <ArrowRight className="ml-1 h-3 w-3" />
                </Link>
              </Button>
            </div>
          </CardHeader>
          <CardContent>
            {isContestsLoading ? (
              <ListSkeleton />
            ) : !contests?.length ? (
              <p className="text-sm text-muted-foreground">
                {t('overview.noContests')}
              </p>
            ) : (
              <div className="space-y-3">
                {contests.map((contest) => {
                  const status = getContestStatus(
                    contest.start_time,
                    contest.end_time,
                    t,
                  );
                  return (
                    <div
                      key={contest.id}
                      className="flex items-center justify-between gap-2"
                    >
                      <Link
                        to={`/contests/${contest.id}`}
                        className="text-sm font-medium hover:text-primary hover:underline truncate"
                      >
                        {contest.title}
                      </Link>
                      <Badge variant={status.variant} className="shrink-0">
                        {status.label}
                      </Badge>
                    </div>
                  );
                })}
              </div>
            )}
          </CardContent>
        </Card>

        {/* Problems */}
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <CardTitle className="flex items-center gap-2">
                <Code2 className="h-4 w-4" />
                {t('overview.problems')}
              </CardTitle>
              <Button variant="ghost" size="sm" asChild>
                <Link to="/problems">
                  {t('overview.viewAll')}
                  <ArrowRight className="ml-1 h-3 w-3" />
                </Link>
              </Button>
            </div>
          </CardHeader>
          <CardContent>
            {isProblemsLoading ? (
              <ListSkeleton />
            ) : !problems?.length ? (
              <p className="text-sm text-muted-foreground">
                {t('overview.noProblems')}
              </p>
            ) : (
              <div className="space-y-3">
                {problems.map((problem) => (
                  <div
                    key={problem.id}
                    className="flex items-center justify-between gap-2"
                  >
                    <div className="flex items-center gap-2 min-w-0">
                      <span className="text-xs text-muted-foreground shrink-0">
                        #{problem.id}
                      </span>
                      <Link
                        to={`/problems/${problem.id}`}
                        className="text-sm font-medium hover:text-primary hover:underline truncate"
                      >
                        {problem.title}
                      </Link>
                    </div>
                    <div className="flex items-center gap-1 text-xs text-muted-foreground shrink-0">
                      <Clock className="h-3 w-3" />
                      {t('result.timeValue', { value: problem.time_limit })}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </CardContent>
        </Card>

        {/* Recent Submissions (logged-in only) */}
        {user && (
          <div className="col-span-2">
            <Card>
              <CardHeader>
                <div className="flex items-start justify-between gap-2">
                  <div>
                    <CardTitle className="flex items-center gap-2">
                      <Activity className="h-4 w-4" />
                      {t('overview.recentSubmissions')}
                    </CardTitle>
                    <CardDescription className="mt-1.5">
                      {t('overview.recentSubmissionsDescription')}
                    </CardDescription>
                  </div>
                  {user.permissions.includes(SUBMISSION_VIEW_ALL) && (
                    <Button variant="ghost" size="sm" asChild>
                      <Link to="/admin/submissions">
                        {t('overview.viewAll')}
                        <ArrowRight className="ml-1 h-3 w-3" />
                      </Link>
                    </Button>
                  )}
                </div>
              </CardHeader>
              <CardContent>
                {isSubmissionsLoading ? (
                  <ListSkeleton />
                ) : !submissions?.length ? (
                  <p className="text-sm text-muted-foreground">
                    {t('overview.noSubmissions')}
                  </p>
                ) : (
                  <div className="rounded-md border overflow-hidden">
                    <SubmissionsTable
                      submissions={submissions}
                      columns={SubmissionsTable.fullColumns}
                      expandable={false}
                      linkBuilder={(sub) =>
                        sub.contest_id
                          ? `/contests/${sub.contest_id}/submissions/${sub.id}`
                          : `/submissions/${sub.id}`
                      }
                    />
                  </div>
                )}
              </CardContent>
            </Card>
          </div>
        )}
      </div>
    </PageLayout>
  );
}
