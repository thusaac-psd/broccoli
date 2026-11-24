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
