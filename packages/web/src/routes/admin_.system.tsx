import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { SYSTEM_VIEW } from '@broccoli/web-sdk/permissions';
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  Skeleton,
} from '@broccoli/web-sdk/ui';
import { AlertCircle, Inbox, Server } from 'lucide-react';

import { PageLayout } from '@/components/PageLayout';
import { Unauthorized } from '@/components/Unauthorized';
import { QueueDepthList } from '@/features/system/components/QueueDepthList';
import { SystemHealthSummary } from '@/features/system/components/SystemHealthSummary';
import { WorkersGrid } from '@/features/system/components/WorkersGrid';
import { useSystemOverview } from '@/features/system/hooks/useSystemOverview';
import { pageTitle, type RouteHandle } from '@/lib/route-title';

const STAT_SKELETONS = ['workers', 'inflight', 'submissions', 'dlq'];

export const handle: RouteHandle = pageTitle('system.title');

export default function AdminSystemPage() {
  const { t } = useTranslation();
  const { user } = useAuth();
  // Gate the poll on the same permission as the render guard below: hooks run
  // before the early Unauthorized return, so without this an unauthorized
  // viewer would 403-loop /admin/system/overview.
  const canViewSystem = !!user?.permissions.includes(SYSTEM_VIEW);
  const { data, isLoading, error } = useSystemOverview({
    enabled: canViewSystem,
  });

  if (!canViewSystem) {
    return <Unauthorized />;
  }

  const totalInFlight = data?.workers.reduce((s, w) => s + w.in_flight, 0) ?? 0;
  const onlineWorkers = data?.workers.length ?? 0;

  return (
    <PageLayout
      pageId="admin-system"
      title={t('system.title')}
      subtitle={t('system.subtitle')}
      icon={<Server className="h-6 w-6 text-primary" />}
    >
      {error && (
        <Card className="border-destructive">
          <CardContent className="pt-6 flex items-start gap-3">
            <AlertCircle className="h-5 w-5 text-destructive shrink-0" />
            <div>
              <p className="text-destructive font-medium">
                {t('system.error.title')}
              </p>
              <p className="text-sm text-muted-foreground mt-1">
                {t('system.error.hint')}
              </p>
            </div>
          </CardContent>
        </Card>
      )}

      {isLoading && !data && (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          {STAT_SKELETONS.map((k) => (
            <Skeleton key={k} className="h-24 w-full" />
          ))}
        </div>
      )}

      {data && (
        <>
          <SystemHealthSummary
            workersOnline={onlineWorkers}
            totalInFlight={totalInFlight}
            submissionsInProgress={data.submissions_in_progress}
            dlqUnresolved={data.dlq_unresolved_count}
          />

          <div className="mt-6 grid gap-6 lg:grid-cols-3">
            <Card className="lg:col-span-2">
              <CardHeader>
                <CardTitle className="flex items-center gap-2 text-base">
                  <Server className="h-4 w-4" />
                  {t('system.workers.title')}
                </CardTitle>
                <CardDescription>
                  {t('system.workers.description')}
                </CardDescription>
              </CardHeader>
              <CardContent>
                <WorkersGrid workers={data.workers} />
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle className="flex items-center gap-2 text-base">
                  <Inbox className="h-4 w-4" />
                  {t('system.queues.title')}
                </CardTitle>
                <CardDescription>
                  {t('system.queues.description')}
                </CardDescription>
              </CardHeader>
              <CardContent>
                <QueueDepthList queues={data.queues} />
              </CardContent>
            </Card>
          </div>
        </>
      )}
    </PageLayout>
  );
}
