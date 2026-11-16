import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { ROLE_MANAGE, USER_MANAGE } from '@broccoli/web-sdk/permissions';
import { Users } from 'lucide-react';

import { PageLayout } from '@/components/PageLayout';
import { Unauthorized } from '@/components/Unauthorized';
import { UserManagementTabs } from '@/features/user-management/components/UserManagementTabs';
import { pageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = pageTitle('users.title');

export default function UserManagementPage() {
  const { t } = useTranslation();
  const { user } = useAuth();

  const canManageUsers = !!user?.permissions.includes(USER_MANAGE);
  const canManageRoles = !!user?.permissions.includes(ROLE_MANAGE);

  if (!user || (!canManageUsers && !canManageRoles)) {
    return <Unauthorized />;
  }

  return (
    <PageLayout
      pageId="users"
      title={t('users.title')}
      subtitle={t('users.subtitle')}
      icon={<Users className="h-6 w-6 text-primary" />}
    >
      <UserManagementTabs
        canManageUsers={canManageUsers}
        canManageRoles={canManageRoles}
      />
    </PageLayout>
  );
}
