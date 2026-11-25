import { expect, type Page, test } from '@playwright/test';

import { mockApi } from './api';
import plugin from './fixture-plugin.json' with { type: 'json' };

/** The print plugin's stations, each token a secret named by its station. */
const SCHEMA = {
  type: 'object',
  properties: {
    stations: {
      type: 'array',
      title: 'Print stations',
      'x-key': 'name',
      items: {
        type: 'object',
        required: ['name', 'token'],
        properties: {
          name: { type: 'string', title: 'Station name' },
          token: { type: 'string', title: 'Token', writeOnly: true },
        },
      },
    },
  },
};

/**
 * The plugin-wide print settings form over a server that stores what each
 * save sends. Returns the bodies of the saves.
 */
async function openStations(page: Page) {
  await mockApi(page, 'admin', { plugins: ['print'] });
  let stored: unknown = {
    stations: [
      { name: 'alpha', token: null },
      { name: 'beta', token: null },
    ],
  };
  const saves: unknown[] = [];
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
              json_schema: SCHEMA,
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
          config: stored,
          secrets: ['stations[0].token', 'stations[1].token'],
          enabled: null,
          position: 0,
          updated_at: '2026-01-01T00:00:00Z',
          json_schema: SCHEMA,
        },
      ],
    }),
  );
  await page.route('**/api/v1/admin/plugins/print/config/print', (route) => {
    const body = route.request().postDataJSON() as { config: unknown };
    saves.push(body.config);
    stored = body.config;
    return route.fulfill({
      json: {
        plugin_id: 'print',
        namespace: 'print',
        config: body.config,
        secrets: [],
        enabled: null,
        position: 0,
        updated_at: '2026-01-01T00:00:00Z',
      },
    });
  });
  await page.goto('/admin/plugins');
  await page.getByRole('button', { name: 'Configure' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.locator('#cfg-stations-1-name')).toHaveValue('beta');
  return { dialog, saves };
}

// Each station's token is kept by its name, so the form saves every kept
// token as null on the station it belongs to, wherever the station moved.
test('a kept secret stays on its station through delete, add and reorder', async ({
  page,
}) => {
  const { dialog, saves } = await openStations(page);
  const save = dialog.getByRole('button', { name: 'Save' });

  await dialog.getByRole('button', { name: 'Remove' }).first().click();
  await save.click();
  await expect.poll(() => saves.length).toBe(1);
  expect(saves[0]).toEqual({ stations: [{ name: 'beta', token: null }] });

  await dialog.getByRole('button', { name: 'Add item' }).click();
  await dialog.locator('#cfg-stations-1-name').fill('gamma');
  await dialog.locator('#cfg-stations-1-token').fill('token-g');
  await save.click();
  await expect.poll(() => saves.length).toBe(2);
  expect(saves[1]).toEqual({
    stations: [
      { name: 'beta', token: null },
      { name: 'gamma', token: 'token-g' },
    ],
  });

  await dialog.getByRole('button', { name: 'Move up' }).nth(1).click();
  await save.click();
  await expect.poll(() => saves.length).toBe(3);
  expect(saves[2]).toEqual({
    stations: [
      { name: 'gamma', token: 'token-g' },
      { name: 'beta', token: null },
    ],
  });
});
