import { useTranslation } from '@broccoli/web-sdk/i18n';
import { usePluginRegistry } from '@broccoli/web-sdk/plugin';
import { Slot } from '@broccoli/web-sdk/slot';
import { Skeleton } from '@broccoli/web-sdk/ui';
import { BarChart3 } from 'lucide-react';

import { PageLayout } from '@/components/PageLayout';

export default function ContestRankingPage() {
  const { t } = useTranslation();
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
    </PageLayout>
  );
}
