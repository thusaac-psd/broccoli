import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Slot } from '@broccoli/web-sdk/slot';
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from '@broccoli/web-sdk/ui';

import { Sidebar } from './Sidebar';

interface AppLayoutProps {
  children: React.ReactNode;
}

const MAIN_ID = 'main-content';

export function AppLayout({ children }: AppLayoutProps) {
  const { t } = useTranslation();
  return (
    <Slot name="app.root" as="div">
      {/* The first Tab stop: keyboard users skip the sidebar's links. */}
      <a
        href={`#${MAIN_ID}`}
        className="sr-only focus:not-sr-only focus:fixed focus:left-2 focus:top-2 focus:z-100 focus:rounded-md focus:bg-background focus:px-3 focus:py-2 focus:text-sm focus:font-medium focus:shadow-md focus:ring-2 focus:ring-ring"
      >
        {t('a11y.skipToContent')}
      </a>
      <SidebarProvider>
        <Sidebar />
        <SidebarInset id={MAIN_ID} tabIndex={-1} className="outline-hidden">
          {/* Phones have no room for the sidebar: a bar above the page
              opens it. It scrolls with the page, so it never covers the
              page's content or its sticky header. */}
          <div className="flex h-11 shrink-0 items-center border-b bg-background px-2 md:hidden">
            <SidebarTrigger className="h-9 w-9" />
          </div>
          <div className="fixed bottom-8 right-8 z-50">
            <Slot name="app.NotificationButton" as="div" />
          </div>
          <div className="flex min-w-0 flex-1 flex-col">{children}</div>
        </SidebarInset>
      </SidebarProvider>
      <Slot name="app.overlay" as="div" />
    </Slot>
  );
}
