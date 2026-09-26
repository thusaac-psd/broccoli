import { useTranslation } from '@broccoli/web-sdk/i18n';
import { usePluginRegistry } from '@broccoli/web-sdk/plugin';
import { Slot } from '@broccoli/web-sdk/slot';
import { Skeleton } from '@broccoli/web-sdk/ui';
import { BarChart3 } from 'lucide-react';

import { PageLayout } from '@/components/PageLayout';

export default function ContestRankingPage() {
  const { t } = useTranslation();
  // The scoreboard is a plugin view: until plugins have loaded, "no
  // participants" would be a false statement.
  const { isLoading } = usePluginRegistry();

  return (
    <PageLayout
      pageId="ranking"
      icon={<BarChart3 className="h-6 w-6 text-sidebar-primary" />}
      title={t('ranking.title')}
      contentClassName="flex flex-col gap-6"
    >
      <Slot name="ranking.header" as="div" />
      <Slot name="ranking.content" as="div" className="w-full">
        {isLoading ? (
          <Skeleton className="h-64 w-full" />
        ) : (
          <div className="rounded-lg border border-dashed p-8 text-center text-sm text-muted-foreground">
            {t('ranking.empty')}
          </div>
        )}
      </Slot>
    </PageLayout>
  );
}
