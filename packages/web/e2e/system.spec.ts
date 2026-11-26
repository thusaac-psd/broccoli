import { expect, test } from '@playwright/test';

import { mockApi, SYSTEM_OVERVIEW } from './api';

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

// What a server lacks reads as the UI language lists names.
for (const [locale, text] of [
  ['en', 'Lacks, while another server serves it: icpc, ioi, and print'],
  ['zh-CN', 'icpc、ioi和print'],
] as const) {
  test(`a server that lacks plugins lists them as ${locale} lists names`, async ({
    page,
  }) => {
    await page.addInitScript(
      (l) => localStorage.setItem('broccoli-locale', l),
      locale,
    );
    await mockApi(page, 'admin');
    await page.route('**/api/v1/admin/system/overview', (route) =>
      route.fulfill({
        json: {
          ...SYSTEM_OVERVIEW,
          servers: [
            {
              server_id: 'server-1',
              ready: false,
              missing: ['icpc', 'ioi', 'print'],
            },
          ],
        },
      }),
    );
    await page.goto('/admin/system');
    await expect(page.getByText(text)).toBeVisible();
  });
}
