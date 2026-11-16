import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Code2 } from 'lucide-react';
import { Outlet } from 'react-router';

import { PageLayout } from '@/components/PageLayout';
import { contestPageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = contestPageTitle('sidebar.submissions');

export default function ContestSubmission() {
  const { t } = useTranslation();

  return (
    <PageLayout
      pageId="contest-submissions"
      title={t('sidebar.submissions')}
      icon={<Code2 className="h-6 w-6 text-primary" />}
    >
      <Outlet />
    </PageLayout>
  );
}
