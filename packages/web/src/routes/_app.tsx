import { ApiClientProvider } from '@broccoli/web-sdk/api';
import { AuthProvider } from '@broccoli/web-sdk/auth';
import { I18nProvider } from '@broccoli/web-sdk/i18n';
import type { LazyPluginLoader } from '@broccoli/web-sdk/plugin';
import { PluginRegistryProvider } from '@broccoli/web-sdk/plugin';
import { ProblemPresentationProvider } from '@broccoli/web-sdk/problem';
import { ThemeProvider, ThemeToaster } from '@broccoli/web-sdk/theme';
import { QueryClientProvider } from '@tanstack/react-query';
import { Outlet } from 'react-router';

import { AppLayout } from '@/components/AppLayout';
import { SlotPermissionsBridge } from '@/components/SlotPermissionsBridge';
import { appConfig } from '@/config';
import { ContestProvider } from '@/features/contest/contexts/contest-context';
import { SubmissionNavigationProvider } from '@/features/submission/contexts/submission-navigation-context';
import { en } from '@/lib/i18n/en';
import { queryClient } from '@/lib/query-client';

// Lazy-loaded plugins - each is code-split into its own chunk by Vite.
const lazyPlugins: LazyPluginLoader[] = [];

export default function AppShell() {
  return (
    <QueryClientProvider client={queryClient}>
      <ApiClientProvider baseUrl={appConfig.api.baseUrl}>
        <I18nProvider defaultLocale="en" coreI18n={{ en }}>
          <ThemeProvider defaultTheme="light">
            <AuthProvider sessionStatusKey={appConfig.api.sessionStatusKey}>
              <ContestProvider>
                <SlotPermissionsBridge>
                  <PluginRegistryProvider
                    backendUrl={appConfig.plugin.backendUrl}
                    lazyPlugins={lazyPlugins}
                  >
                    <ProblemPresentationProvider>
                      <SubmissionNavigationProvider>
                        <AppLayout>
                          <Outlet />
                        </AppLayout>
                      </SubmissionNavigationProvider>
                    </ProblemPresentationProvider>
                    <ThemeToaster richColors closeButton />
                  </PluginRegistryProvider>
                </SlotPermissionsBridge>
              </ContestProvider>
            </AuthProvider>
          </ThemeProvider>
        </I18nProvider>
      </ApiClientProvider>
    </QueryClientProvider>
  );
}
