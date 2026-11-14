import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { CONTEST_MANAGE } from '@broccoli/web-sdk/permissions';
import { Trophy } from 'lucide-react';

import { PageLayout } from '@/components/PageLayout';
import { Unauthorized } from '@/components/Unauthorized';
import { AdminContestsTab } from '@/features/admin/components/AdminContestsTab';
import { pageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = pageTitle('contests.title');

export default function ContestListPage() {
  const { t } = useTranslation();
  const { user } = useAuth();

  if (!user || !user.permissions.includes(CONTEST_MANAGE)) {
    return <Unauthorized />;
  }

  return (
    <PageLayout
      pageId="contests"
      title={t('contests.title')}
      icon={<Trophy className="h-6 w-6 text-primary" />}
    >
      <AdminContestsTab />
    </PageLayout>
  );
}
