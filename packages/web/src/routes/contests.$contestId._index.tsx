import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { CONTEST_MANAGE } from '@broccoli/web-sdk/permissions';
import { QueryErrorState } from '@broccoli/web-sdk/ui';
import { Trophy } from 'lucide-react';
import { useParams } from 'react-router';

import { PageLayout } from '@/components/PageLayout';
import { ContestAdminActions } from '@/features/contest/components/ContestAdminActions';
import { ContestCountdown } from '@/features/contest/components/ContestCountdown';
import { ContestEnrollCard } from '@/features/contest/components/ContestEnrollCard';
import { ContestProblemsCard } from '@/features/contest/components/ContestProblemsCard';
import { useContestEnroll } from '@/features/contest/hooks/use-contest-enroll';
import { useContestInfo } from '@/features/contest/hooks/use-contest-info';
import { contestPageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = contestPageTitle();

export default function ContestOverviewPage() {
  const { t } = useTranslation();
  const { user } = useAuth();
  const { contestId } = useParams();
  const id = Number(contestId);
  const { contest, query: contestQuery } = useContestInfo(id);
  const canManageContest = !!user?.permissions.includes(CONTEST_MANAGE);
  const enrollState = useContestEnroll({
    contestId: id,
    contest,
    canManageContest,
  });

  if (!contestId || Number.isNaN(id)) {
    return (
      <div className="flex flex-col gap-4 p-6">
        <h1 className="text-2xl font-bold">{t('contests.notFound')}</h1>
      </div>
    );
  }

  if (!contest && contestQuery.isError) {
    return (
      <PageLayout
        pageId="contest-overview"
        title={t('contests.title')}
        icon={<Trophy className="h-6 w-6 text-primary" />}
      >
        <QueryErrorState
          error={contestQuery.error}
          retry={() => void contestQuery.refetch()}
          retrying={contestQuery.isFetching}
          notFound={t('contests.notFound')}
          failure={t('contests.loadFailed')}
        />
      </PageLayout>
    );
  }

  return (
    <PageLayout
      pageId="contest-overview"
      title={contest?.title ?? t('contests.title')}
      subtitle={contest?.description}
      icon={<Trophy className="h-6 w-6 text-primary" />}
      contentClassName="flex flex-col gap-4"
    >
      <div className="grid grid-cols-1 gap-4 md:grid-cols-[1fr_20rem]">
        <div className="min-w-0 space-y-4">
          <ContestProblemsCard contestId={id} />
        </div>
        <div className="space-y-4 md:sticky md:top-6 h-fit">
          <ContestCountdown />
          {enrollState.canShowEnrollCard && (
            <ContestEnrollCard
              onEnroll={enrollState.enroll}
              isPending={enrollState.isPending}
            />
          )}
          {enrollState.canShowUnregisterButton && (
            <ContestEnrollCard
              onEnroll={enrollState.enroll}
              isPending={enrollState.isPending}
              onUnregister={enrollState.unregister}
              isUnregistering={enrollState.isUnregistering}
              showUnregister
            />
          )}
          <ContestAdminActions />
        </div>
      </div>
    </PageLayout>
  );
}
