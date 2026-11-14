import { expect, test } from '@playwright/test';

import { mockApi } from './api';

// Every route's code loads once the app has started (so navigating never
// needs the server for it), which makes whatever a route module imports
// statically part of every page's download. The heavy libraries (Monaco,
// KaTeX, the editors) stay behind dynamic imports: they load on the page that
// shows them, never with the route code.
test('loading every route module loads no editor or math code', async ({
  page,
}) => {
  await mockApi(page, 'guest');
  const scripts: string[] = [];
  page.on('request', (request) => {
    const { pathname } = new URL(request.url());
    if (pathname.endsWith('.js')) scripts.push(pathname);
  });
  await page.goto('/login');
  await page.waitForLoadState('networkidle');

  // The modules the idle preload imports, imported here so the test waits
  // for each one's whole static graph.
  await page.evaluate(async () => {
    const { routes } = (
      window as unknown as {
        __reactRouterManifest: { routes: Record<string, { module: string }> };
      }
    ).__reactRouterManifest;
    await Promise.all(
      Object.values(routes).map(({ module }) => import(module)),
    );
  });

  expect(
    await page.evaluate(() => 'MonacoEnvironment' in self),
    'Monaco was set up',
  ).toBe(false);
  // A dynamically imported module is a chunk of its own, named after it.
  expect(
    scripts.filter((path) =>
      /\/(editor\.api|[\w-]+\.impl|markdown-math)[\w.-]*\.js$/.test(path),
    ),
  ).toEqual([]);
});
