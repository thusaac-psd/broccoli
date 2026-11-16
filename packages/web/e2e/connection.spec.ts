import { expect, test } from '@playwright/test';

import { mockApi } from './api';

const REFRESH = '**/api/v1/auth/refresh';

// A page loaded while the server cannot answer (a gateway in front of a dead
// API server) says so and offers a retry instead of waiting forever, and it
// continues by itself once the server answers again.
test('a reload while the server is unreachable says so and recovers', async ({
  page,
}) => {
  await mockApi(page, 'contestant');
  await page.route(REFRESH, (route) =>
    route.fulfill({ status: 502, body: 'Bad Gateway' }),
  );
  await page.goto('/contests/1/qa');

  await expect(
    page.getByRole('heading', { name: 'Connection lost' }),
  ).toBeVisible();

  await page.unroute(REFRESH);
  await page.getByRole('button', { name: 'Try again' }).click();
  await expect(page.getByRole('heading', { name: 'Q & A' })).toBeVisible();
  await expect(
    page.getByRole('heading', { name: 'Connection lost' }),
  ).toHaveCount(0);
});
