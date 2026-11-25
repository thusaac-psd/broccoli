import { expect, type Page, test } from '@playwright/test';

import { mockApi } from './api';
import registries from './fixture-registries.json' with { type: 'json' };

// A form field chosen from what the plugins registered keeps its stored
// value whatever the registries say: one this server does not offer shows as
// unavailable, and nothing is cleared while they load or after they fail.
// Saving an unrelated field never rewrites it, since a problem's type,
// checker format and contest type are judging inputs.

/** The PATCH bodies the problem edit form sends. */
async function recordSaves(page: Page): Promise<unknown[]> {
  const saves: unknown[] = [];
  await page.route('**/api/v1/problems/1', (route) => {
    if (route.request().method() !== 'PATCH') return route.fallback();
    saves.push(route.request().postDataJSON());
    return route.fulfill({ json: {} });
  });
  return saves;
}

async function renameAndSave(page: Page, { opened = false } = {}) {
  if (!opened) await page.getByRole('button', { name: 'Edit' }).click();
  const title = page.getByLabel('Title');
  await expect(title).toHaveValue(/.+/);
  await title.fill('Renamed');
  await page.getByRole('button', { name: 'Save' }).click();
}

test('a problem whose types this server does not offer shows them as unavailable and keeps them', async ({
  page,
}) => {
  await mockApi(page, 'admin');
  await page.route('**/api/v1/plugins/registries', (route) =>
    route.fulfill({
      json: {
        ...registries,
        problem_types: ['communication'],
        checker_formats: ['lines'],
        contest_types: ['ioi'],
      },
    }),
  );
  const saves = await recordSaves(page);
  await page.goto('/problems/1');
  await page.getByRole('button', { name: 'Edit' }).click();

  for (const id of ['problem-type', 'checker-format', 'default-contest-type']) {
    await expect(page.locator(`#${id}`)).toContainText('(unavailable)');
  }
  await page.locator('#problem-type').click();
  await expect(
    page.getByRole('option', { name: /\(unavailable\)/ }),
  ).toBeVisible();
  await expect(page.getByRole('option')).toHaveCount(2);
  await page.keyboard.press('Escape');

  await page.getByLabel('Title').fill('Renamed');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect.poll(() => saves).toEqual([{ title: 'Renamed' }]);
});

test('a problem saved while the registries fail keeps its types', async ({
  page,
}) => {
  await mockApi(page, 'admin');
  await page.route('**/api/v1/plugins/registries', (route) =>
    route.fulfill({
      status: 403,
      json: { code: 'FORBIDDEN', message: 'no' },
    }),
  );
  const saves = await recordSaves(page);
  await page.goto('/problems/1');
  await page.getByRole('button', { name: 'Edit' }).click();
  await expect(page.locator('#problem-type')).toHaveText('batch');
  await renameAndSave(page, { opened: true });
  await expect.poll(() => saves).toEqual([{ title: 'Renamed' }]);
});

test('a problem saved before the registries answer keeps its types', async ({
  page,
}) => {
  await mockApi(page, 'admin');
  // The registries never answer.
  await page.route('**/api/v1/plugins/registries', () => {});
  const saves = await recordSaves(page);
  await page.goto('/problems/1');
  await renameAndSave(page);
  await expect.poll(() => saves).toEqual([{ title: 'Renamed' }]);
});

test('a contest whose type this server does not offer shows it as unavailable', async ({
  page,
}) => {
  await mockApi(page, 'admin');
  await page.route('**/api/v1/plugins/registries', (route) =>
    route.fulfill({ json: { ...registries, contest_types: ['ioi'] } }),
  );
  await page.goto('/contests/1');
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  const select = page.getByRole('dialog').getByLabel('Contest type');
  await expect(select.locator('option:checked')).toHaveText(/\(unavailable\)/);
});

test('the worker picker whose overview cannot load says so instead of hiding', async ({
  page,
}) => {
  await mockApi(page, 'admin');
  await page.route('**/api/v1/admin/system/overview', (route) =>
    route.fulfill({ status: 500, json: { code: 'INTERNAL', message: '' } }),
  );
  await page.goto('/problems/1?tab=coding');
  await expect(page.getByText('Pin to:')).toBeVisible();
  await expect(
    page.getByRole('alert').filter({ hasText: 'Something went wrong' }),
  ).toBeVisible();
});
