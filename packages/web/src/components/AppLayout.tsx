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
          <div className="fixed left-0 top-1/2 z-50 -translate-y-1/2 md:hidden">
            <SidebarTrigger className="relative h-11 w-5 rounded-r-md rounded-l-none border border-l-0 border-border bg-muted/20 shadow-xs hover:bg-accent [&>svg]:hidden after:absolute after:inset-y-1.5 after:left-1/2 after:w-[2px] after:-translate-x-1/2 after:rounded-full after:bg-foreground/70" />
          </div>
          <div className="fixed bottom-8 right-8 z-50">
            <Slot name="app.NotificationButton" as="div" />
          </div>
          <div className="flex flex-1 flex-col">{children}</div>
        </SidebarInset>
      </SidebarProvider>
      <Slot name="app.overlay" as="div" />
    </Slot>
  );
}
