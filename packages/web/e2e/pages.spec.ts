import AxeBuilder from '@axe-core/playwright';
import { expect, type Page, test } from '@playwright/test';

import { mockApi, type Viewer } from './api';

// The pages each kind of visitor opens, checked in both themes: no WCAG A/AA
// violation axe can find, a name in the browser tab, the document language
// of the UI, nothing wider than a phone screen, and no request the server
// would refuse.
const PAGES: Record<Viewer, string[]> = {
  guest: ['/', '/login', '/register'],
  admin: [
    '/admin',
    '/admin/system',
    '/admin/plugins',
    '/admin/submissions',
    '/admin/users',
    '/admin/dlq',
    '/contests',
    '/problems',
    '/problems/1',
    '/contests/1',
    '/contests/1/problems/1',
    '/contests/1/submissions',
    '/contests/1/rankings',
    '/contests/1/qa',
    '/submissions/1',
  ],
  contestant: [
    '/contests/1',
    '/contests/1/problems/2',
    '/contests/1/submissions',
    '/contests/2',
  ],
};

// Pages and slots of the plugins this repository ships, checked like the
// host's: each page opens with every frontend plugin loaded.
const FRONTEND_PLUGINS = [
  'print',
  'icpc',
  'ioi',
  'codelink-bracket',
  'codelink-qualifier',
  'cooldown',
  'standard-languages',
  'communication-evaluator',
];
const PLUGIN_PAGES: Record<Viewer, string[]> = {
  guest: [],
  admin: ['/print-queue', '/admin/plugins', '/contests/1/rankings'],
  contestant: ['/contests/1/rankings', '/contests/1/problems/2'],
};

const APP_NAME = 'Broccoli OJ';

async function open(
  page: Page,
  viewer: Viewer,
  path: string,
  theme: string,
  plugins: string[] = [],
) {
  const unanswered = await mockApi(page, viewer, { plugins });
  const failures: string[] = [];
  page.on('response', (response) => {
    if (response.status() >= 400 && response.url().includes('/api/')) {
      failures.push(`${response.status()} ${response.url()}`);
    }
  });
  page.on('console', (message) => {
    if (message.type() === 'error') failures.push(message.text());
  });
  page.on('pageerror', (error) => failures.push(error.message));
  await page.addInitScript((t) => localStorage.setItem('theme', t), theme);
  await page.goto(path);
  await page.waitForLoadState('networkidle');
  return { unanswered, failures };
}

const CASES = [
  ...(Object.entries(PAGES) as [Viewer, string[]][]).flatMap(
    ([viewer, paths]) =>
      paths.map((path) => ({ viewer, path, plugins: [] as string[] })),
  ),
  ...(Object.entries(PLUGIN_PAGES) as [Viewer, string[]][]).flatMap(
    ([viewer, paths]) =>
      paths.map((path) => ({ viewer, path, plugins: FRONTEND_PLUGINS })),
  ),
];

for (const { viewer, path, plugins } of CASES) {
  for (const theme of ['light', 'dark']) {
    const name = `${viewer} ${path}${plugins.length ? ' with plugins' : ''}`;
    test(`${name} (${theme})`, async ({ page }, info) => {
      const { unanswered, failures } = await open(
        page,
        viewer,
        path,
        theme,
        plugins,
      );

      const { violations } = await new AxeBuilder({ page })
        .withTags(['wcag2a', 'wcag2aa'])
        .analyze();
      // Soft: a failing page reports every problem, not just the first.
      expect
        .soft(
          violations.map((v) => ({
            rule: v.id,
            nodes: v.nodes.map((n) => n.target.join(' ')),
          })),
        )
        .toEqual([]);

      await expect
        .soft(page)
        .toHaveTitle(
          path === '/' && viewer === 'guest'
            ? APP_NAME
            : new RegExp(`. · ${APP_NAME}$`),
        );
      await expect.soft(page.locator('html')).toHaveAttribute('lang', 'en');

      // Toggles in a list say which item they toggle.
      const toggles = (await page.locator('body').ariaSnapshot())
        .split('\n')
        .map((line) => /- (?:switch|checkbox) "(.*)"/.exec(line)?.[1])
        .filter((name) => name !== undefined);
      expect
        .soft(
          toggles.filter((name, i) => toggles.indexOf(name) !== i),
          'toggles that share a name',
        )
        .toEqual([]);

      if (info.project.name === 'phone') {
        const overflow = await page.evaluate(
          () =>
            document.documentElement.scrollWidth -
            document.documentElement.clientWidth,
        );
        expect.soft(overflow, 'page scrolls sideways').toBe(0);
      }

      expect.soft(unanswered, 'API calls the mock does not answer').toEqual([]);
      expect(failures).toEqual([]);
    });
  }
}

test('the document language and title follow the UI locale', async ({
  page,
}) => {
  await mockApi(page, 'admin');
  await page.addInitScript(() =>
    localStorage.setItem('broccoli-locale', 'zh-CN'),
  );
  await page.goto('/admin/plugins');
  await expect(page.locator('html')).toHaveAttribute('lang', 'zh-CN');
  await expect(page).toHaveTitle(/^插件管理 · /);
});

test('switching the UI locale switches the document language and title', async ({
  page,
}, info) => {
  test.skip(info.project.name === 'phone', 'the sidebar is a sheet there');
  await mockApi(page, 'admin');
  await page.goto('/admin/plugins');
  await expect(page.locator('html')).toHaveAttribute('lang', 'en');
  await page.getByRole('button', { name: 'English' }).click();
  await page.getByRole('menuitem', { name: '中文' }).click();
  await expect(page.locator('html')).toHaveAttribute('lang', 'zh-CN');
  await expect(page).toHaveTitle(/^插件管理 · /);
});
