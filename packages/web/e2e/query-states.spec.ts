import { expect, test } from '@playwright/test';

import { mockApi, NOW } from './api';

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

  await page.clock.runFor(120_000);
  await expect(page.getByRole('button', { name: 'Retry' })).toBeVisible();
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
  await page.clock.runFor(10_000);
  await expect(page.getByRole('button', { name: 'Retry' })).toBeVisible();
  await expect(page.getByText(/no contests available/)).toHaveCount(0);
});
