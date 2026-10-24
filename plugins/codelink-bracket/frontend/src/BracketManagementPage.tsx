import { useApiClient } from '@broccoli/web-sdk/api';
import { useAuth, useAuthReady, viewerCacheKey } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { CONTEST_MANAGE } from '@broccoli/web-sdk/permissions';
import { Button, Skeleton } from '@broccoli/web-sdk/ui';
import { useQuery } from '@tanstack/react-query';
import { ArrowLeft, CalendarDays } from 'lucide-react';
import { Link, useMatch, useNavigate } from 'react-router';

import { useBracket } from './hooks/useBracket';
import { MANAGEMENT_ROUTE } from './lib/links';
import { SetupPanel } from './SetupPanel';
import { StageSchedule } from './StageSchedule';

export function BracketManagementPage() {
  const { t } = useTranslation();
  const { user } = useAuth();
  const authReady = useAuthReady();
  const api = useApiClient();
  const navigate = useNavigate();
  const route = useMatch(MANAGEMENT_ROUTE);
  const contestId = Number(route?.params.contestId);
  const validId = Number.isSafeInteger(contestId) && contestId > 0;
  const canManage = user?.permissions.includes(CONTEST_MANAGE) ?? false;
  const contest = useQuery({
    queryKey: [
      'codelink-bracket-management-contest',
      contestId,
      viewerCacheKey(user),
    ],
    enabled: authReady && validId && canManage,
    queryFn: async ({ signal }) => {
      const { data, error } = await api.GET('/contests/{id}', {
        params: { path: { id: contestId } },
        signal,
      });
      if (error) throw error;
      return data;
    },
  });
  const bracket = useBracket(validId ? contestId : undefined, {
    enabled: canManage && contest.data?.contest_type === 'codelink-bracket',
  });
  if (!authReady)
    return (
      <div className="p-6">
        <Skeleton className="h-64" />
      </div>
    );
  if (!canManage)
    return (
      <p role="alert" className="p-6 text-sm text-muted-foreground">
        {t('codelink-bracket.manage.forbidden')}
      </p>
    );
  if (
    !validId ||
    contest.isError ||
    (contest.data && contest.data.contest_type !== 'codelink-bracket')
  )
    return (
      <p role="alert" className="p-6">
        {t('codelink-bracket.manage.notFound')}
      </p>
    );
  if (!contest.data)
    return (
      <div className="p-6">
        <Skeleton className="h-64" />
      </div>
    );
  const home = `/contests/${contestId}`;
  return (
    <div className="p-6 space-y-6">
      <header className="flex flex-wrap items-center justify-between gap-3 border-b pb-4">
        <div>
          <h1 className="flex items-center gap-3 text-2xl font-bold tracking-tight">
            <CalendarDays className="h-6 w-6 text-sidebar-primary" />
            {t('codelink-bracket.manage.title')}
          </h1>
          <p className="mt-2 text-sm text-muted-foreground">
            {contest.data.title}
          </p>
        </div>
        <Button asChild variant="outline" size="sm">
          <Link to={home}>
            <ArrowLeft className="mr-1.5 h-4 w-4" />
            {t('codelink-bracket.manage.back')}
          </Link>
        </Button>
      </header>
      {bracket.data?.stages && (
        <StageSchedule
          stages={bracket.data.stages}
          thirdPlace={bracket.data.third_place_enabled ?? false}
        />
      )}
      <SetupPanel contestId={contestId} onClose={() => navigate(home)} />
    </div>
  );
}
