import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from '@broccoli/web-sdk/ui';
import { Printer } from 'lucide-react';
import { Link, useLocation } from 'react-router';

const ROUTE = '/print-queue';

// The slot's `contest:manage` permission gates this for staff only.
export function PrintNavLink() {
  const { t } = useTranslation();
  const { pathname } = useLocation();
  const isActive = pathname.startsWith(ROUTE);

  return (
    <SidebarGroup>
      <SidebarGroupLabel>{t('print.nav.groupLabel')}</SidebarGroupLabel>
      <SidebarGroupContent>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              asChild
              isActive={isActive}
              tooltip={t('print.queue.nav')}
            >
              <Link to={ROUTE}>
                <Printer
                  className={isActive ? 'text-sidebar-primary' : undefined}
                />
                <span>{t('print.queue.nav')}</span>
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroupContent>
    </SidebarGroup>
  );
}
