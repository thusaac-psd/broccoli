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

// The dialog names a namespace's tab by its short title (its id when it has
// none), with its description below, and each array item by its key field,
// or its number while it has none, never by the array's own title.
test('config tabs and array items read as names, not ids', async ({ page }) => {
  await mockApi(page, 'admin', { plugins: ['print'] });
  await page.addInitScript(() =>
    localStorage.setItem('broccoli-locale', 'zh-CN'),
  );
  const stations = {
    type: 'object',
    title: 'Stations',
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
  // The tab reads the title in the plugin's pack, its schema's title else.
  await expect(
    dialog.getByRole('tab', { name: '打印', exact: true }),
  ).toBeVisible();
  await expect(
    dialog.getByRole('tab', { name: 'other', exact: true }),
  ).toBeVisible();
  await expect(dialog.getByRole('tab', { name: '代码打印设置' })).toHaveCount(
    0,
  );
  await expect(dialog.getByText('代码打印设置')).toBeVisible();
  await expect(dialog.getByRole('heading', { name: 'desk' })).toBeVisible();
  await expect(dialog.getByRole('heading', { name: '第 2 项' })).toBeVisible();
  await expect(dialog.getByText('打印站', { exact: true })).toHaveCount(1);
});

// A boolean left at its default reads in the user's language, never as a
// raw true or false.
test('an unset boolean reads its default in words', async ({ page }) => {
  await mockApi(page, 'admin', { plugins: ['print'] });
  await page.addInitScript(() =>
    localStorage.setItem('broccoli-locale', 'zh-CN'),
  );
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
              json_schema: {
                type: 'object',
                properties: {
                  require_approval: {
                    type: 'boolean',
                    title: 'Require approval',
                    default: false,
                  },
                },
              },
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
  await page.getByRole('button', { name: '配置' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByText('(否)')).toBeVisible();
  await expect(dialog.getByText('(false)')).toHaveCount(0);
});

// An enum's options, and its default, read as their titles, never as their
// raw values.
test('enum options read as their titles', async ({ page }) => {
  await mockApi(page, 'admin', { plugins: ['print'] });
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
              json_schema: {
                type: 'object',
                properties: {
                  mode: {
                    type: 'string',
                    title: 'Mode',
                    enum: ['redirect', 'fifo_args'],
                    'x-enum-titles': [
                      'Standard input and output',
                      'FIFO paths as arguments',
                    ],
                    default: 'redirect',
                  },
                },
              },
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
  const select = page.getByRole('dialog').locator('#cfg-mode');
  await expect(select.locator('option')).toHaveText([
    'Standard input and output',
    'Standard input and output',
    'FIFO paths as arguments',
  ]);
  await expect(select.locator('option', { hasText: 'redirect' })).toHaveCount(
    0,
  );
});
