import { useTranslation } from '@broccoli/web-sdk/i18n';
import { usePluginRegistry } from '@broccoli/web-sdk/plugin';
import { Skeleton } from '@broccoli/web-sdk/ui';
import { matchPath, useLocation } from 'react-router';

import { ErrorCatcher } from '@/components/ErrorCatcher';
import type { RouteHandle } from '@/lib/route-title';

/** The registered plugin route the current URL matches, if any. */
function useMatchedPluginRoute() {
  const { routes } = usePluginRegistry();
  const { pathname } = useLocation();
  return routes.find((route) =>
    matchPath({ path: route.path, caseSensitive: false, end: true }, pathname),
  );
}

/** A plugin page is named by the title key its manifest declares. */
export const handle: RouteHandle = {
  useTitle: function usePluginPageTitle() {
    const { t } = useTranslation();
    const route = useMatchedPluginRoute();
    return route ? t(route.title) : undefined;
  },
};

/**
 * Renders the plugin page whose registered route matches the current URL,
 * or a 404 when none does.
 */
export default function ExtensionPage() {
  const { t } = useTranslation();
  const { components, isLoading } = usePluginRegistry();
  const matchedRoute = useMatchedPluginRoute();

  // Show loading state while plugins are still being loaded
  if (isLoading) {
    return (
      <div className="flex flex-col gap-6 p-6">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-64 w-full" />
      </div>
    );
  }

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
