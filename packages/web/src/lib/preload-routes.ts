// Loads every route's code once the app has started, so moving to another
// page later never depends on the server being up to fetch code: with the
// server gone, a route module that has not loaded yet makes React Router
// reload the page, onto the browser's own error page. The modules are the
// ones React Router itself imports (from its manifest), so they are fetched
// once and shared.

interface RouteManifest {
  routes: Record<string, { module: string }>;
}

declare global {
  interface Window {
    __reactRouterManifest?: RouteManifest;
  }
}

export function preloadRouteModules(): void {
  const routes = window.__reactRouterManifest?.routes;
  if (!routes) return;
  const load = () => {
    for (const { module } of Object.values(routes)) {
      // A failure here is harmless: the route loads (and retries) when it is
      // visited.
      void import(/* @vite-ignore */ module).catch(() => undefined);
    }
  };
  if ('requestIdleCallback' in window) window.requestIdleCallback(load);
  else setTimeout(load, 0);
}
