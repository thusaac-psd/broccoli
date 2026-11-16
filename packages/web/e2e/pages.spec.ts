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

const APP_NAME = 'Broccoli OJ';

async function open(page: Page, viewer: Viewer, path: string, theme: string) {
  const unanswered = await mockApi(page, viewer);
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

for (const [viewer, paths] of Object.entries(PAGES) as [Viewer, string[]][]) {
  for (const path of paths) {
    for (const theme of ['light', 'dark']) {
      test(`${viewer} ${path} (${theme})`, async ({ page }, info) => {
        const { unanswered, failures } = await open(page, viewer, path, theme);

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

        if (info.project.name === 'phone') {
          const overflow = await page.evaluate(
            () =>
              document.documentElement.scrollWidth -
              document.documentElement.clientWidth,
          );
          expect.soft(overflow, 'page scrolls sideways').toBe(0);
        }

        expect
          .soft(unanswered, 'API calls the mock does not answer')
          .toEqual([]);
        expect(failures).toEqual([]);
      });
    }
  }
}

test('a contest whose plugin is disabled says so on every page', async ({
  page,
}) => {
  await mockApi(page, 'contestant');
  const notice =
    'Results for this contest are unavailable while its plugin is disabled.';

  await page.goto('/contests/3');
  await expect(page.getByText(notice)).toBeVisible();
  await expect(
    page.getByRole('link', { name: 'Unavailable', exact: true }),
  ).toBeVisible();

  await page.goto('/contests/3/problems/1');
  await expect(page.getByText(notice).first()).toBeVisible();
  await expect(page.getByRole('button', { name: /submit/i })).toHaveCount(0);

  await page.goto('/contests/3/rankings');
  await expect(page.getByText(notice)).toBeVisible();
  await expect(page.getByText('This contest has no scoreboard')).toHaveCount(0);

  await page.goto('/contests/3/submissions');
  await expect(
    page
      .getByText("Result unavailable while the contest's plugin is disabled")
      .first(),
  ).toBeVisible();
});

test('a guest asking for a scoreboard is asked to sign in', async ({
  page,
}) => {
  await mockApi(page, 'guest');
  await page.goto('/contests/1/rankings');
  await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible();
  await expect(page.getByText('This contest has no scoreboard')).toHaveCount(0);
});

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
