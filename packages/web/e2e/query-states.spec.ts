import { expect, test } from '@playwright/test';

import { mockApi, NOW, runClockUntil } from './api';

const UNAVAILABLE = {
  status: 503,
  json: { code: 'SERVICE_UNAVAILABLE', message: 'unavailable' },
};

// A list that cannot be loaded says so, with a retry: it never shows as
// empty, and it never stays loading while the server keeps failing.

test('the print queue with its backend down says it could not load the jobs', async ({
  page,
}) => {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'admin', { plugins: ['print'] });
  await page.route('**/api/v1/p/print/**', (route) =>
    route.fulfill(UNAVAILABLE),
  );
  await page.goto('/print-queue');
  await expect(
    page.getByRole('heading', { name: 'Print Queue' }),
  ).toBeVisible();

  // A busy server is waited out: six retries, at most 16 s apart.
  await runClockUntil(page, () =>
    expect(page.getByRole('button', { name: 'Retry' })).toBeVisible({
      timeout: 500,
    }),
  );
  await expect(page.getByText('No print jobs')).toHaveCount(0);
});

test('the home page with the contest list failing does not say there are no contests', async ({
  page,
}) => {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant');
  await page.route('**/api/v1/contests?**', (route) =>
    route.fulfill({ status: 500, json: { code: 'INTERNAL', message: '' } }),
  );
  await page.goto('/');
  await runClockUntil(page, () =>
    expect(page.getByRole('button', { name: 'Retry' })).toBeVisible({
      timeout: 500,
    }),
  );
  await expect(page.getByText(/no contests available/)).toHaveCount(0);
});

// A first load that fails shows its failure at once, with what the server
// said, and is asked again behind it: never skeletons, a dash or a loading
// line for the whole outage, and never a Submit for a problem not loaded.
for (const [name, path, api] of [
  ['a problem', '/problems/1', /\/api\/v1\/problems\/1(\?|$)/],
  [
    'a contest problem',
    '/contests/1/problems/1',
    /\/api\/v1\/contests\/1\/problems\/1(\?|$)/,
  ],
  [
    'the clarifications',
    '/contests/1/qa',
    /\/api\/v1\/contests\/1\/clarifications(\?|$)/,
  ],
] as const) {
  test(`${name} whose first load fails shows the failure while it is asked again`, async ({
    page,
  }) => {
    await page.clock.install({ time: NOW });
    await mockApi(page, 'contestant');
    let down = true;
    await page.route(api, (route) =>
      down
        ? route.fulfill({
            status: 503,
            headers: { 'retry-after': '2' },
            json: { code: 'DATABASE_UNAVAILABLE', message: 'unavailable' },
          })
        : route.fallback(),
    );
    await page.goto(path);
    // The first answer is a failure: well before the retries are spent.
    await runClockUntil(
      page,
      () =>
        expect(
          page
            .getByText("The server's database is unavailable right now")
            .first(),
        ).toBeVisible({ timeout: 500 }),
      { stepMs: 1_000, steps: 8 },
    );
    await expect(page.getByRole('button', { name: 'Submit' })).toHaveCount(0);
    await expect(page.getByText('Loading…')).toHaveCount(0);

    down = false;
    await runClockUntil(page, () =>
      expect(
        page.getByText("The server's database is unavailable right now"),
      ).toHaveCount(0, {
        timeout: 500,
      }),
    );
  });
}

// A setting or list that cannot load is never shown as an answer: the
// registration switch never reads "closed", a contest's sidebar never reads
// as a contest with no problems, and a filter's choices say they are
// incomplete.

test('the registration switch whose setting cannot load says so instead of reading closed', async ({
  page,
}) => {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'admin');
  await page.route('**/api/v1/auth/registration', (route) =>
    route.fulfill({ status: 500, json: { code: 'INTERNAL', message: '' } }),
  );
  await page.goto('/admin/users');
  await runClockUntil(page, () =>
    expect(
      page.getByRole('alert').filter({ hasText: 'Something went wrong' }),
    ).toBeVisible({ timeout: 500 }),
  );
  await expect(page.getByRole('switch', { name: /registration/i })).toHaveCount(
    0,
  );
});

test("a contest's sidebar whose problems cannot load offers to load them again", async ({
  page,
}, info) => {
  test.skip(info.project.name === 'phone', 'the sidebar is a sheet there');
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant');
  let down = true;
  await page.route('**/api/v1/contests/1/problems', (route) =>
    down
      ? route.fulfill({ status: 500, json: { code: 'INTERNAL', message: '' } })
      : route.fallback(),
  );
  await page.goto('/contests/1/qa');
  const retry = page.getByRole('button', {
    name: 'Problems could not load: retry',
  });
  await runClockUntil(page, () => expect(retry).toBeVisible({ timeout: 500 }));
  down = false;
  await retry.click();
  await expect(retry).toHaveCount(0);
  await expect(
    page.getByRole('link', { name: /Sum of Two Integers/ }),
  ).toBeVisible();
});

test("the contest submissions' problem filter whose problems cannot load says so", async ({
  page,
}) => {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'admin');
  await page.route('**/api/v1/contests/1/problems', (route) =>
    route.fulfill({ status: 500, json: { code: 'INTERNAL', message: '' } }),
  );
  await page.goto('/contests/1/submissions?problem=2');
  // The applied filter keeps its choice, named by its id until the problems
  // load, and the list says it is incomplete.
  const filter = page.getByRole('button').filter({ hasText: /^2$/ });
  await expect(filter).toBeVisible();
  await runClockUntil(page, async () => {
    await filter.click();
    try {
      await expect(
        page.getByRole('menuitem', {
          name: 'All problems (the list could not load)',
        }),
      ).toBeVisible({ timeout: 500 });
    } finally {
      await page.keyboard.press('Escape');
    }
  });
});
