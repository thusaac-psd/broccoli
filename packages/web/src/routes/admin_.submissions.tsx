import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { SUBMISSION_VIEW_ALL } from '@broccoli/web-sdk/permissions';
import { Activity } from 'lucide-react';

import { PageLayout } from '@/components/PageLayout';
import { Unauthorized } from '@/components/Unauthorized';
import { AllSubmissions } from '@/features/submission/components/AllSubmissions';
import { pageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = pageTitle('adminSubmissions.title');

export default function AdminSubmissionsPage() {
  const { t } = useTranslation();
  const { user } = useAuth();

  if (!user || !user.permissions.includes(SUBMISSION_VIEW_ALL)) {
    return <Unauthorized />;
  }

  return (
    <PageLayout
      pageId="admin-submissions"
      title={t('adminSubmissions.title')}
      subtitle={t('adminSubmissions.subtitle')}
      icon={<Activity className="h-6 w-6 text-primary" />}
    >
      <AllSubmissions />
    </PageLayout>
  );
}
