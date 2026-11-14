import { AuthProvider, NavigateContext } from '@broccoli/web-sdk/auth';
import { PluginRegistryProvider } from '@broccoli/web-sdk/plugin';
import { ProblemPresentationProvider } from '@broccoli/web-sdk/problem';
import { ThemeProvider, ThemeToaster } from '@broccoli/web-sdk/theme';
import { Outlet, useNavigate } from 'react-router';

import { AppLayout } from '@/components/AppLayout';
import { BaseProviders } from '@/components/BaseProviders';
import { SessionExpiredPrompt } from '@/components/SessionExpiredPrompt';
import { SlotPermissionsBridge } from '@/components/SlotPermissionsBridge';
import { appConfig } from '@/config';
import { ContestProvider } from '@/features/contest/contexts/contest-context';
import { SubmissionNavigationProvider } from '@/features/submission/contexts/submission-navigation-context';

export default function AppShell() {
  const navigate = useNavigate();
  return (
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
                      <AppLayout>
                        <Outlet />
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
  );
}
