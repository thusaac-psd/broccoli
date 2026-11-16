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

// Choosing an archive is a new choice every time, the same file included,
// and a publish refused because the plugin changed meanwhile drops the
// summary it no longer matches.
test('the upload dialog inspects the same archive again, and drops a summary a conflict outdated', async ({
  page,
}) => {
  await mockApi(page, 'admin');
  let inspections = 0;
  await page.route('**/api/v1/admin/plugins/upload/inspect', (route) => {
    inspections += 1;
    if (inspections === 1) {
      return route.fulfill({
        status: 422,
        json: { code: 'PLUGIN_LOAD_FAILED', message: 'bad manifest' },
      });
    }
    return route.fulfill({
      json: {
        id: 'demo',
        name: 'Demo',
        version: '1.0.0',
        description: null,
        size_bytes: 2048,
        archive_hash: 'abc123',
        current: null,
        has_server: true,
        has_web: false,
        permissions: [],
        sql_read: [],
        incompatible_configs: [],
      },
    });
  });
  await page.route('**/api/v1/admin/plugins/upload', (route) =>
    route.request().method() === 'POST'
      ? route.fulfill({
          status: 409,
          json: { code: 'CONFLICT', message: 'the plugin changed' },
        })
      : route.fallback(),
  );
  await page.goto('/admin/plugins');
  await page.getByRole('button', { name: 'Upload plugin' }).click();
  const dialog = page.getByRole('dialog', { name: 'Upload a plugin' });
  const archive = {
    name: 'demo.tar.gz',
    mimeType: 'application/gzip',
    buffer: Buffer.from('not really an archive'),
  };
  const input = dialog.getByTestId('plugin-archive-input');

  await input.setInputFiles(archive);
  await expect(
    dialog.getByText('The plugin could not be loaded.', { exact: false }),
  ).toBeVisible();
  await input.setInputFiles(archive);
  await expect(dialog.getByTestId('plugin-archive-summary')).toBeVisible();
  expect(inspections).toBe(2);

  await dialog.getByRole('button', { name: 'Publish' }).click();
  await expect(dialog.getByTestId('plugin-archive-summary')).toHaveCount(0);
  await expect(dialog.getByRole('button', { name: 'Publish' })).toBeDisabled();
});
