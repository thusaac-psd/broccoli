import { useTranslation } from '@broccoli/web-sdk/i18n';
import { usePluginRegistry } from '@broccoli/web-sdk/plugin';
import { Skeleton } from '@broccoli/web-sdk/ui';
import { matchPath, useLocation } from 'react-router';

import { ErrorCatcher } from '@/components/ErrorCatcher';

/**
 * ExtensionPage
 * Handles dynamic routing for plugins.
 * It matches the current URL against registered plugin routes.
 */
export default function ExtensionPage() {
  const { t } = useTranslation();
  const { routes, components, isLoading } = usePluginRegistry();
  const location = useLocation();

  // Show loading state while plugins are still being loaded
  if (isLoading) {
    return (
      <div className="flex flex-col gap-6 p-6">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-64 w-full" />
      </div>
    );
  }

  // Find the first route that matches the current pathname
  // We use matchPath to support parameters like /contest/:id
  const matchedRoute = routes.find((route) =>
    matchPath(
      { path: route.path, caseSensitive: false, end: true },
      location.pathname,
    ),
  );

  if (!matchedRoute) {
    return <ErrorCatcher code="404" />;
  }

  const Component = components[matchedRoute.component];

  if (!Component) {
    return (
      <div className="p-8 text-center text-destructive">
        <h1 className="text-xl font-bold">{t('extension.componentError')}</h1>
        <p>
          {t('extension.componentMissing', {
            component: matchedRoute.component,
          })}
        </p>
      </div>
    );
  }

  return <Component />;
}
