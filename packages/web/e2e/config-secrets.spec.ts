import { expect, type Page, test } from '@playwright/test';

import { mockApi } from './api';
import plugin from './fixture-plugin.json' with { type: 'json' };

/**
 * The print plugin's stations, each token a secret named by its station:
 * one each station requires, unless `optional`.
 */
function stationsSchema(optional = false) {
  return {
    type: 'object',
    properties: {
      stations: {
        type: 'array',
        title: 'Print stations',
        'x-key': 'name',
        items: {
          type: 'object',
          required: optional ? ['name'] : ['name', 'token'],
          properties: {
            name: { type: 'string', title: 'Station name' },
            token: { type: 'string', title: 'Token', writeOnly: true },
          },
        },
      },
    },
  };
}

/**
 * `config` as the server answers it once stored: each token typed gets a
 * new ref, each ref sent back stays, and each cleared one is gone.
 */
function storedAs(config: unknown, mint: () => string): unknown {
  const { stations } = config as { stations: Record<string, unknown>[] };
  return {
    stations: stations.map(({ token, ...station }) => {
      if (typeof token === 'string') {
        return { ...station, token: { $secret: mint() } };
      }
      const kept =
        typeof token === 'object' && token !== null && '$secret' in token;
      return kept ? { ...station, token } : station;
    }),
  };
}

/**
 * The plugin-wide print settings form over a server that stores what each
 * save sends, each set token shown as its ref. Returns the bodies of the
 * saves.
 */
async function openStations(
  page: Page,
  { optional = false, refuse }: { optional?: boolean; refuse?: unknown } = {},
) {
  const SCHEMA = stationsSchema(optional);
  await mockApi(page, 'admin', { plugins: ['print'] });
  let stored: unknown = {
    stations: [
      { name: 'alpha', token: { $secret: 'ra' } },
      { name: 'beta', token: { $secret: 'rb' } },
    ],
  };
  let minted = 0;
  const mint = () => `ref-${++minted}`;
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
    if (refuse) return route.fulfill({ status: 400, json: refuse });
    stored = storedAs(body.config, mint);
    return route.fulfill({
      json: {
        plugin_id: 'print',
        namespace: 'print',
        config: stored,
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

// Each station's token is kept by the ref a read showed it as, so the form
// sends every kept token's ref back on the station it belongs to, wherever
// the station moved.
test('a kept secret stays on its station through delete, add and reorder', async ({
  page,
}) => {
  const { dialog, saves } = await openStations(page);
  const save = dialog.getByRole('button', { name: 'Save' });

  await dialog.getByRole('button', { name: 'Remove' }).first().click();
  await save.click();
  await expect.poll(() => saves.length).toBe(1);
  expect(saves[0]).toEqual({
    stations: [{ name: 'beta', token: { $secret: 'rb' } }],
  });

  await dialog.getByRole('button', { name: 'Add item' }).click();
  await dialog.locator('#cfg-stations-1-name').fill('gamma');
  await dialog.locator('#cfg-stations-1-token').fill('token-g');
  await save.click();
  await expect.poll(() => saves.length).toBe(2);
  expect(saves[1]).toEqual({
    stations: [
      { name: 'beta', token: { $secret: 'rb' } },
      { name: 'gamma', token: 'token-g' },
    ],
  });

  await dialog.getByRole('button', { name: 'Move up' }).nth(1).click();
  await save.click();
  await expect.poll(() => saves.length).toBe(3);
  // Saved, gamma's token reads as the ref the server minted: kept, never
  // sent again.
  expect(saves[2]).toEqual({
    stations: [
      { name: 'gamma', token: { $secret: 'ref-1' } },
      { name: 'beta', token: { $secret: 'rb' } },
    ],
  });
});

// Typing over a stored secret and emptying the input again keeps it.
test('an emptied secret input keeps the stored secret', async ({ page }) => {
  const { dialog, saves } = await openStations(page);
  const token = dialog.locator('#cfg-stations-0-token');
  await token.fill('typed');
  await token.fill('');
  await expect(token).toHaveAttribute(
    'placeholder',
    'Leave empty to keep the stored secret',
  );
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect.poll(() => saves.length).toBe(1);
  expect(saves[0]).toEqual({
    stations: [
      { name: 'alpha', token: { $secret: 'ra' } },
      { name: 'beta', token: { $secret: 'rb' } },
    ],
  });
});

// A stored secret is cleared only on purpose: the form sends the explicit
// marker, and taking the clear back keeps the stored secret.
test('a stored secret is cleared with its own action, which can be taken back', async ({
  page,
}) => {
  const { dialog, saves } = await openStations(page, { optional: true });
  const save = dialog.getByRole('button', { name: 'Save' });
  const clear = dialog.getByRole('button', { name: 'Clear' });

  await clear.first().click();
  await expect(dialog.locator('#cfg-stations-0-token')).toHaveAttribute(
    'placeholder',
    'Cleared when you save',
  );
  await dialog.getByRole('button', { name: 'Keep it' }).click();

  await clear.nth(1).click();
  await save.click();
  await expect.poll(() => saves.length).toBe(1);
  expect(saves[0]).toEqual({
    stations: [
      { name: 'alpha', token: { $secret: 'ra' } },
      { name: 'beta', token: { $clear: true } },
    ],
  });
});

// Typing a new secret over a pending clear replaces the clear.
test('typing a secret replaces a pending clear', async ({ page }) => {
  const { dialog, saves } = await openStations(page, { optional: true });
  await dialog.getByRole('button', { name: 'Clear' }).first().click();
  await dialog.locator('#cfg-stations-0-token').fill('token-new');
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect.poll(() => saves.length).toBe(1);
  expect(saves[0]).toEqual({
    stations: [
      { name: 'alpha', token: 'token-new' },
      { name: 'beta', token: { $secret: 'rb' } },
    ],
  });
});

// A secret every station requires is replaced, never cleared.
test('a required secret offers no clear', async ({ page }) => {
  const { dialog } = await openStations(page);
  await expect(dialog.locator('#cfg-stations-0-token')).toHaveAttribute(
    'placeholder',
    'Leave empty to keep the stored secret',
  );
  await expect(dialog.getByRole('button', { name: 'Clear' })).toHaveCount(0);
});

// A station removed and added again, or renamed, is a new station: none of
// a stored station's token is its own, so its token reads as new and is
// entered before the form saves.
test('a re-added or renamed station has no stored secret to keep', async ({
  page,
}) => {
  const { dialog, saves } = await openStations(page);
  await dialog.getByRole('button', { name: 'Remove' }).first().click();
  await dialog.getByRole('button', { name: 'Add item' }).click();
  await dialog.locator('#cfg-stations-1-name').fill('alpha');
  await dialog.locator('#cfg-stations-0-name').fill('beta-renamed');
  for (const i of [0, 1]) {
    await expect(
      dialog.locator(`#cfg-stations-${i}-token`),
    ).not.toHaveAttribute(
      'placeholder',
      'Leave empty to keep the stored secret',
    );
  }
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect(
    dialog.getByText('Required', { exact: true }).first(),
  ).toBeVisible();
  expect(saves).toEqual([]);

  await dialog.locator('#cfg-stations-1-token').fill('token-a2');
  await dialog.locator('#cfg-stations-0-token').fill('token-b2');
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect.poll(() => saves.length).toBe(1);
  expect(saves[0]).toEqual({
    stations: [
      { name: 'beta-renamed', token: 'token-b2' },
      { name: 'alpha', token: 'token-a2' },
    ],
  });
});

// A refusal names the field by its titles and its item, not its raw path.
test('a refused field is named by its title and its item', async ({ page }) => {
  const { dialog } = await openStations(page, {
    refuse: {
      code: 'VALIDATION_ERROR',
      message: 'invalid',
      details: {
        fields: [
          {
            path: 'stations[1].token',
            message: 'this write never read the secret stored for this item',
            kind: 'secret_not_stored',
          },
        ],
      },
    },
  });
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect(dialog.getByText('Print stations › beta › Token')).toBeVisible();
  await expect(dialog.getByText('stations.1.token')).toHaveCount(0);
});
