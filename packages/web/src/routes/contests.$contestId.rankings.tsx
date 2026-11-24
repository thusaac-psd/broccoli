import { useTranslation } from '@broccoli/web-sdk/i18n';
import { usePluginRegistry } from '@broccoli/web-sdk/plugin';
import { Slot } from '@broccoli/web-sdk/slot';
import { QueryView, Skeleton } from '@broccoli/web-sdk/ui';
import { BarChart3 } from 'lucide-react';
import { useParams } from 'react-router';

import { PageLayout } from '@/components/PageLayout';
import { PluginOutageNotice } from '@/features/contest/components/PluginOutageNotice';
import { useContestInfo } from '@/features/contest/hooks/use-contest-info';
import { contestPageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = contestPageTitle('ranking.title');

export default function ContestRankingPage() {
  const { t } = useTranslation();
  const { contestId } = useParams();
  const { query } = useContestInfo(Number(contestId));
  // The scoreboard is a plugin view. Until plugins have loaded nothing can be
  // said; after, an empty slot means this contest's format shows none, which
  // says nothing about its participants.
  const { isLoading } = usePluginRegistry();

  return (
    <PageLayout
      pageId="ranking"
      icon={<BarChart3 className="h-6 w-6 text-sidebar-primary" />}
      title={t('ranking.title')}
      contentClassName="flex flex-col gap-6"
    >
      {/* The contest is read first: a guest is asked to sign in, a contest
          that does not exist says so, and one whose plugin is out says that
          instead of showing no scoreboard. */}
      <QueryView
        query={query}
        loading={<Skeleton className="h-64 w-full" />}
        notFound={t('contests.notFound')}
        failure={t('contests.loadFailed')}
      >
        {(contest) =>
          contest.plugin_outage ? (
            <PluginOutageNotice outage={contest.plugin_outage} />
          ) : (
            <>
              <Slot name="ranking.header" as="div" />
              <Slot name="ranking.content" as="div" className="w-full">
                {isLoading ? (
                  <Skeleton className="h-64 w-full" />
                ) : (
                  <div className="rounded-lg border border-dashed p-8 text-center text-sm text-muted-foreground">
                    {t('ranking.unavailable')}
                  </div>
                )}
              </Slot>
            </>
          )
        }
      </QueryView>
    </PageLayout>
  );
}
