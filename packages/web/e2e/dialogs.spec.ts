import { expect, test } from '@playwright/test';

import { mockApi } from './api';

// A dialog belongs to the page it opened on: moving to another page closes
// it, so it never covers the page it led to.
test('the session-ended prompt closes once it has led to the sign-in page', async ({
  page,
}) => {
  await mockApi(page, 'contestant');
  await page.route('**/api/v1/auth/refresh', (route) =>
    route.fulfill({
      status: 401,
      json: { code: 'TOKEN_INVALID', message: '' },
    }),
  );
  await page.goto('/contests/1');
  const prompt = page.getByRole('dialog', { name: 'Your session has ended' });
  await expect(prompt).toBeVisible();

  await prompt.getByRole('button', { name: 'Sign in' }).click();
  await expect(page).toHaveURL(/\/login\?next=/);
  await expect(prompt).toHaveCount(0);
  await page.getByLabel('Username').fill('alice');
});
