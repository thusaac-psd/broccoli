import { ApiClientProvider } from '@broccoli/web-sdk/api';
import { I18nProvider } from '@broccoli/web-sdk/i18n';
import { QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

import { appConfig } from '@/config';
import { embeddedCatalogs } from '@/lib/i18n/packs.gen';
import { queryClient } from '@/lib/query-client';

/**
 * The providers translated text needs: the app shell mounts everything else
 * inside them, and the root error page mounts only these, so it speaks the
 * user's language from the same (already cached) translations.
 */
export function BaseProviders({ children }: { children: ReactNode }) {
  return (
    <QueryClientProvider client={queryClient}>
      <ApiClientProvider baseUrl={appConfig.api.baseUrl}>
        <I18nProvider defaultLocale="en" embeddedCatalogs={embeddedCatalogs}>
          {children}
        </I18nProvider>
      </ApiClientProvider>
    </QueryClientProvider>
  );
}
