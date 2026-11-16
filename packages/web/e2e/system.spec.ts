import { expect, test } from '@playwright/test';

import { mockApi } from './api';

test('the system page says no worker takes work, counts the queued backlog and no refusing worker as capacity', async ({
  page,
}) => {
  await mockApi(page, 'admin');
  await page.goto('/admin/system');
  await expect(page.getByText('No worker takes work')).toBeVisible();
  const stat = (label: string) =>
    page
      .getByText(label, { exact: true })
      .locator('xpath=following-sibling::p');
  await expect(stat('Workers taking work')).toHaveText('0');
  await expect(stat('Queued submissions')).toHaveText('3');
  await expect(stat('Parked submissions')).toHaveText('0');
  await expect(page.getByText('not taking work')).toBeVisible();
});
