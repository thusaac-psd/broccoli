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
