import { expect, type Page, type Route, test } from '@playwright/test';

import { mockApi, NOW, PLUGIN_ROUTES } from './api';

// A print dialog closes only once its job is queued, cannot be dismissed
// while it is being sent, and never closes or clears on a timer: a dialog
// reopened right after a print keeps what the contestant types.

async function openPrinting(page: Page) {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant', {
    plugins: ['print'],
    pluginRoutes: [
      ...PLUGIN_ROUTES,
      [
        'GET',
        /^\/p\/print\/api\/plugins\/print\/jobs\/mine$/,
        () => ({ data: [] }),
      ],
    ],
  });
}

const QUEUED = {
  status: 201,
  json: { ok: true, jobs: 1, pages: 2, status: 'pending' },
};

test('a print dialog cannot be dismissed while its job is being sent', async ({
  page,
}) => {
  await openPrinting(page);
  let send: Route | undefined;
  await page.route('**/api/plugins/print/contests/1/jobs', (route) => {
    send = route;
  });
  await page.goto('/contests/1/problems/1?tab=coding');
  await page.getByRole('button', { name: 'Print code' }).click();
  const dialog = page.getByRole('dialog', { name: 'Print code' });
  await dialog.getByLabel('Code').fill('int main() {}');
  await dialog.getByRole('button', { name: 'Print' }).click();
  await expect.poll(() => send !== undefined).toBe(true);

  await page.keyboard.press('Escape');
  await page.clock.runFor(100);
  await expect(dialog.getByLabel('Code')).toHaveValue('int main() {}');

  await send?.fulfill(QUEUED);
  await expect(dialog).toHaveCount(0);
  await expect(page.getByText('Sent to the print queue')).toBeVisible();
  await expect(page.getByText('1 file(s), ~2 page(s)')).toBeVisible();
});

test('a print dialog reopened right after a print keeps what is typed', async ({
  page,
}) => {
  await openPrinting(page);
  await page.route('**/api/plugins/print/contests/1/jobs', (route) =>
    route.fulfill(QUEUED),
  );
  await page.goto('/contests/1/problems/1?tab=coding');
  const dialog = page.getByRole('dialog', { name: 'Print code' });
  await page.getByRole('button', { name: 'Print code' }).click();
  await dialog.getByLabel('Code').fill('int main() {}');
  await dialog.getByRole('button', { name: 'Print' }).click();
  await expect(page.getByText('Sent to the print queue').first()).toBeVisible();
  // Closing it again is harmless: it is closed once the job is queued.
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);

  await page.getByRole('button', { name: 'Print code' }).click();
  await dialog.getByLabel('Code').fill('second draft');
  await page.clock.runFor(5_000);
  await expect(dialog.getByLabel('Code')).toHaveValue('second draft');
});

test('a print the plugin refuses says what the plugin said, not a generic retry hint', async ({
  page,
}) => {
  await openPrinting(page);
  await page.route('**/api/plugins/print/contests/1/jobs', (route) =>
    route.fulfill({
      status: 429,
      json: {
        code: 'print.RATE_LIMITED',
        message: 'Print limit reached (2 jobs per contestant)',
      },
    }),
  );
  await page.goto('/contests/1/problems/1?tab=coding');
  await page.getByRole('button', { name: 'Print code' }).click();
  const dialog = page.getByRole('dialog', { name: 'Print code' });
  await dialog.getByLabel('Code').fill('int main() {}');
  await dialog.getByRole('button', { name: 'Print' }).click();
  await expect(
    dialog.getByText(
      'Could not send print job (Print limit reached (2 jobs per contestant))',
    ),
  ).toBeVisible();
  await expect(dialog.getByText('Too many requests')).toHaveCount(0);
});
