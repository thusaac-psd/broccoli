import { expect, test } from '@playwright/test';

import { mockApi } from './api';
import plugin from './fixture-plugin.json' with { type: 'json' };

// A field left at its default shows that default as its placeholder, for a
// number as for any other kind of field.
test('a field at its default shows the default as its placeholder', async ({
  page,
}) => {
  await mockApi(page, 'admin', { plugins: ['print'] });
  const schema = {
    type: 'object',
    properties: {
      max_pages: { type: 'integer', title: 'Max pages', default: 10 },
      ratio: { type: 'number', title: 'Ratio', default: 0.25 },
      banner: { type: 'string', title: 'Banner', default: 'Welcome' },
    },
  };
  await page.route('**/api/v1/admin/plugins', (route) =>
    route.fulfill({
      json: [
        {
          ...plugin,
          config_schemas: [
            {
              namespace: 'print',
              description: 'Code printing settings',
              scopes: ['plugin'],
              json_schema: schema,
            },
          ],
        },
      ],
    }),
  );
  await page.route('**/api/v1/admin/plugins/print/config', (route) =>
    route.fulfill({ json: [] }),
  );
  await page.goto('/admin/plugins');
  await page.getByRole('button', { name: 'Configure' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.locator('#cfg-max_pages')).toHaveAttribute(
    'placeholder',
    '10',
  );
  await expect(dialog.locator('#cfg-ratio')).toHaveAttribute(
    'placeholder',
    '0.25',
  );
  await expect(dialog.locator('#cfg-banner')).toHaveAttribute(
    'placeholder',
    'Welcome',
  );
});

// The dialog names a namespace by its description in the user's language,
// never its raw id, and each array item by its key field, or its number
// while it has none, never by the array's own title.
test('config tabs and array items read as names, not ids', async ({ page }) => {
  await mockApi(page, 'admin', { plugins: ['print'] });
  await page.addInitScript(() =>
    localStorage.setItem('broccoli-locale', 'zh-CN'),
  );
  const stations = {
    type: 'object',
    properties: {
      stations: {
        type: 'array',
        title: 'Print stations',
        description: 'Each entry authorizes one print station.',
        'x-key': 'name',
        items: {
          type: 'object',
          properties: { name: { type: 'string', title: 'Station name' } },
        },
      },
    },
  };
  await page.route('**/api/v1/admin/plugins', (route) =>
    route.fulfill({
      json: [
        {
          ...plugin,
          config_schemas: [
            {
              namespace: 'print',
              description: 'Code printing settings',
              scopes: ['plugin'],
              json_schema: stations,
            },
            {
              namespace: 'other',
              description: 'Other settings',
              scopes: ['plugin'],
              json_schema: { type: 'object', properties: {} },
            },
          ],
        },
      ],
    }),
  );
  await page.route('**/api/v1/admin/plugins/print/config', (route) =>
    route.fulfill({
      json: [
        {
          plugin_id: 'print',
          namespace: 'print',
          config: { stations: [{ name: 'desk' }, { name: '' }] },
          secrets: [],
          config_withheld: false,
          enabled: null,
          position: 0,
          updated_at: '2026-01-01T00:00:00Z',
        },
      ],
    }),
  );
  await page.goto('/admin/plugins');
  await page.getByRole('button', { name: '配置' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByRole('tab', { name: '代码打印设置' })).toBeVisible();
  await expect(
    dialog.getByRole('tab', { name: 'Other settings' }),
  ).toBeVisible();
  await expect(
    dialog.getByRole('tab', { name: 'print', exact: true }),
  ).toHaveCount(0);
  await expect(dialog.getByRole('heading', { name: 'desk' })).toBeVisible();
  await expect(dialog.getByRole('heading', { name: '第 2 项' })).toBeVisible();
  await expect(dialog.getByText('打印站', { exact: true })).toHaveCount(1);
});
