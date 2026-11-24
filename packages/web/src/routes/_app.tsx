import { AuthProvider, NavigateContext } from '@broccoli/web-sdk/auth';
import { PluginRegistryProvider } from '@broccoli/web-sdk/plugin';
import { ProblemPresentationProvider } from '@broccoli/web-sdk/problem';
import { ThemeProvider, ThemeToaster } from '@broccoli/web-sdk/theme';
import { PageContext } from '@broccoli/web-sdk/ui';
import { Outlet, useLocation, useNavigate } from 'react-router';

import { AppLayout } from '@/components/AppLayout';
import { BaseProviders } from '@/components/BaseProviders';
import { DocumentTitle } from '@/components/DocumentTitle';
import { SessionExpiredPrompt } from '@/components/SessionExpiredPrompt';
import { SessionGate } from '@/components/SessionGate';
import { SlotPermissionsBridge } from '@/components/SlotPermissionsBridge';
import { appConfig } from '@/config';
import { ContestProvider } from '@/features/contest/contexts/contest-context';
import { SubmissionNavigationProvider } from '@/features/submission/contexts/submission-navigation-context';
import { useFollowSubmissionVisibility } from '@/features/submission/hooks/use-follow-submission-visibility';
import { queryClient } from '@/lib/query-client';

export default function AppShell() {
  const navigate = useNavigate();
  // Dialogs and sheets close when the page changes.
  const { pathname } = useLocation();
  useFollowSubmissionVisibility(queryClient);
  return (
    <PageContext value={pathname}>
      <NavigateContext value={navigate}>
        <BaseProviders>
          <ThemeProvider defaultTheme="light">
            <AuthProvider sessionStatusKey={appConfig.api.sessionStatusKey}>
              <ContestProvider>
                <SlotPermissionsBridge>
                  <PluginRegistryProvider
                    backendUrl={appConfig.plugin.backendUrl}
                  >
                    <ProblemPresentationProvider>
                      <SubmissionNavigationProvider>
                        <DocumentTitle />
                        <AppLayout>
                          <SessionGate>
                            <Outlet />
                          </SessionGate>
                        </AppLayout>
                        <SessionExpiredPrompt />
                      </SubmissionNavigationProvider>
                    </ProblemPresentationProvider>
                    <ThemeToaster richColors closeButton />
                  </PluginRegistryProvider>
                </SlotPermissionsBridge>
              </ContestProvider>
            </AuthProvider>
          </ThemeProvider>
        </BaseProviders>
      </NavigateContext>
    </PageContext>
  );
}
