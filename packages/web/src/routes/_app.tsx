import { AuthProvider } from '@broccoli/web-sdk/auth';
import { PluginRegistryProvider } from '@broccoli/web-sdk/plugin';
import { ProblemPresentationProvider } from '@broccoli/web-sdk/problem';
import { ThemeProvider, ThemeToaster } from '@broccoli/web-sdk/theme';
import { Outlet } from 'react-router';

import { AppLayout } from '@/components/AppLayout';
import { BaseProviders } from '@/components/BaseProviders';
import { SlotPermissionsBridge } from '@/components/SlotPermissionsBridge';
import { appConfig } from '@/config';
import { ContestProvider } from '@/features/contest/contexts/contest-context';
import { SubmissionNavigationProvider } from '@/features/submission/contexts/submission-navigation-context';
import { DocumentTitle } from '@/lib/route-title';

export default function AppShell() {
  return (
    <BaseProviders>
      <ThemeProvider defaultTheme="light">
        <AuthProvider sessionStatusKey={appConfig.api.sessionStatusKey}>
          <ContestProvider>
            <SlotPermissionsBridge>
              <PluginRegistryProvider backendUrl={appConfig.plugin.backendUrl}>
                <ProblemPresentationProvider>
                  <SubmissionNavigationProvider>
                    <DocumentTitle />
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
    </BaseProviders>
  );
}
