import { expect, type Page, test } from '@playwright/test';

import { mockApi, NOW, PLUGIN_ROUTES } from './api';

// A plugin that gates Submit on the problem page reads its state for the
// problem on screen, from the server, and counts a wait down by the time that
// has passed, not by how many timer ticks the browser ran.

const COOLDOWN =
  /^\/p\/cooldown\/api\/plugins\/cooldown\/contests\/1\/problems\/(\d+)\/status$/;
const LIMIT =
  /^\/p\/submission-limit\/api\/plugins\/submission-limit\/contests\/1\/problems\/(\d+)\/status$/;

const problemOf = (url: URL) => Number(url.pathname.split('/').at(-2));

/** Waits per problem, as the cooldown's status route answers them. */
async function openCooldown(page: Page, waits: Record<number, number>) {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant', {
    plugins: ['cooldown'],
    pluginRoutes: [
      ...PLUGIN_ROUTES,
      [
        'GET',
        COOLDOWN,
        (url) => ({
          enabled: true,
          cooldown_seconds: 60,
          wait_seconds: waits[problemOf(url)] ?? 0,
          source: 'contest',
        }),
      ],
    ],
  });
}

const submit = (page: Page) => page.getByRole('button', { name: 'Submit' });

test('a submission in flight blocks Submit for the wait the server reports', async ({
  page,
}) => {
  await openCooldown(page, { 1: 30 });
  await page.goto('/contests/1/problems/1?tab=coding');
  await expect(page.getByText('Wait 30s')).toBeVisible();
  await expect(submit(page)).toBeDisabled();
});

test('a cooldown counts down by elapsed time when the timers run late', async ({
  page,
}) => {
  await openCooldown(page, { 1: 30 });
  await page.goto('/contests/1/problems/1?tab=coding');
  await expect(page.getByText('Wait 30s')).toBeVisible();
  // A hidden tab runs a due timer once, however much time passed.
  await page.clock.fastForward(25_000);
  await expect(page.getByText('Wait 5s')).toBeVisible();
});

/**
 * Opens problem 2's coding tab, then problem 1's, then goes back two steps to
 * problem 2's: the problem changes under the same coding tab.
 */
async function backToProblemTwo(page: Page, problemOne: string) {
  test.skip(
    test.info().project.name === 'phone',
    'the problem links are in the collapsed sidebar',
  );
  await page.goto('/contests/1/problems/2?tab=coding');
  await page
    .getByRole('link', { name: /Sum of Two Integers/ })
    .first()
    .click();
  await page.getByRole('button', { name: 'Submit' }).first().click();
  await expect(page.getByText(problemOne)).toBeVisible();
  await expect(submit(page)).toBeDisabled();
  await page.evaluate(() => history.go(-2));
  await expect(page).toHaveURL(/\/contests\/1\/problems\/2\?tab=coding/);
}

const UNAVAILABLE = {
  status: 503,
  json: { code: 'SERVICE_UNAVAILABLE', message: 'unavailable' },
};

test("one problem's cooldown does not block Submit on the next problem", async ({
  page,
}) => {
  await openCooldown(page, { 1: 30 });
  await page.route('**/cooldown/contests/1/problems/2/status', (route) =>
    route.fulfill(UNAVAILABLE),
  );
  await backToProblemTwo(page, 'Wait 30s');
  await expect(page.getByText('Wait 30s')).toHaveCount(0);
  await expect(submit(page)).toBeEnabled();
});

test("one problem's submission limit does not block Submit on the next problem", async ({
  page,
}) => {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant', {
    plugins: ['submission-limit'],
    pluginRoutes: [
      ...PLUGIN_ROUTES,
      [
        'GET',
        LIMIT,
        () => ({
          enabled: true,
          submissions_made: 5,
          max_submissions: 5,
          remaining: 0,
          unlimited: false,
          source: 'contest',
        }),
      ],
    ],
  });
  await page.route(
    '**/submission-limit/contests/1/problems/2/status',
    (route) => route.fulfill(UNAVAILABLE),
  );
  await backToProblemTwo(page, '5 / 5');
  await expect(page.getByText('5 / 5')).toHaveCount(0);
  await expect(submit(page)).toBeEnabled();
});
