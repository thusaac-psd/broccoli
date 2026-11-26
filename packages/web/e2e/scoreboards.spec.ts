import { expect, type Page, test } from '@playwright/test';

import { mockApi, NOW, PLUGIN_ROUTES, runClockUntil } from './api';

// A contest-format plugin's scoreboard and contest card say when they cannot
// load or refresh: a failed call never reads as "this contest has no
// scoreboard", and a board that stopped refreshing never passes for current.

const UNAVAILABLE = {
  status: 503,
  json: { code: 'SERVICE_UNAVAILABLE', message: 'unavailable' },
};
const NO_SCOREBOARD = 'This contest has no scoreboard to show.';

const ICPC = '**/api/v1/p/icpc/api/plugins/icpc/contests/1';
const IOI = '**/api/v1/p/ioi/api/plugins/ioi/contests/4';

async function open(page: Page, plugin: 'icpc' | 'ioi' | 'codelink-bracket') {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant', { plugins: [plugin] });
}

test('an IOI scoreboard names its columns as the contest labels its problems', async ({
  page,
}) => {
  await open(page, 'ioi');
  await page.goto('/contests/4/rankings');
  await expect(
    page.getByRole('columnheader', { name: 'C', exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole('columnheader', { name: 'A', exact: true }),
  ).toHaveCount(0);
});

test('an ICPC scoreboard whose contest info fails still shows the board', async ({
  page,
}) => {
  await open(page, 'icpc');
  await page.route(`${ICPC}/info`, (route) => route.fulfill(UNAVAILABLE));
  await page.goto('/contests/1/rankings');
  await expect(page.getByRole('cell', { name: 'Alice' })).toBeVisible();
  await expect(page.getByText(NO_SCOREBOARD)).toHaveCount(0);
});

test('an ICPC scoreboard that cannot load says so, with a retry', async ({
  page,
}) => {
  await open(page, 'icpc');
  await page.route(`${ICPC}/standings`, (route) => route.fulfill(UNAVAILABLE));
  await page.goto('/contests/1/rankings');
  await runClockUntil(page, () =>
    expect(page.getByRole('button', { name: 'Retry' })).toBeVisible({
      timeout: 500,
    }),
  );
  await expect(page.getByText(NO_SCOREBOARD)).toHaveCount(0);
});

test('an IOI scoreboard that cannot load says so, with a retry', async ({
  page,
}) => {
  await open(page, 'ioi');
  await page.route(`${IOI}/**`, (route) => route.fulfill(UNAVAILABLE));
  await page.goto('/contests/4/rankings');
  await runClockUntil(page, () =>
    expect(
      page.getByRole('button', { name: 'Retry', exact: true }),
    ).toBeVisible({ timeout: 500 }),
  );
  await expect(page.getByText(NO_SCOREBOARD)).toHaveCount(0);
});

test('the ICPC contest card whose info cannot load says so, with a retry', async ({
  page,
}) => {
  await open(page, 'icpc');
  await page.route(`${ICPC}/info`, (route) => route.fulfill(UNAVAILABLE));
  await page.goto('/contests/1');
  await runClockUntil(page, () =>
    expect(page.getByRole('button', { name: 'Retry' })).toBeVisible({
      timeout: 500,
    }),
  );
});

test('the IOI contest card whose info cannot load says so, with a retry', async ({
  page,
}) => {
  await open(page, 'ioi');
  await page.route(`${IOI}/info`, (route) => route.fulfill(UNAVAILABLE));
  await page.goto('/contests/4');
  await runClockUntil(page, () =>
    expect(
      page.getByRole('button', { name: 'Retry', exact: true }),
    ).toBeVisible({ timeout: 500 }),
  );
});

for (const [plugin, path, url] of [
  ['icpc', '/contests/1/rankings', `${ICPC}/standings`],
  ['ioi', '/contests/4/rankings', `${IOI}/scoreboard`],
] as const) {
  test(`a live ${plugin.toUpperCase()} scoreboard whose refreshes fail says it is not current`, async ({
    page,
  }) => {
    await open(page, plugin);
    await page.goto(path);
    await expect(page.getByRole('cell', { name: 'Alice' })).toBeVisible();

    await page.route(url, (route) => route.fulfill(UNAVAILABLE));
    await runClockUntil(page, () =>
      expect(page.getByText('Could not refresh')).toBeVisible({
        timeout: 500,
      }),
    );
    await expect(page.getByRole('cell', { name: 'Alice' })).toBeVisible();
  });
}

test('a Codelink bracket whose refreshes fail keeps the bracket and says it is not current', async ({
  page,
}) => {
  await open(page, 'codelink-bracket');
  await page.goto('/contests/5/rankings');
  const bracket = page.getByText('The bracket has not been set up yet.');
  await expect(bracket).toBeVisible();

  await page.route(
    '**/api/v1/p/codelink-bracket/api/plugins/codelink-bracket/contests/5/bracket',
    (route) => route.fulfill(UNAVAILABLE),
  );
  await runClockUntil(page, () =>
    expect(page.getByText('Could not refresh')).toBeVisible({ timeout: 500 }),
  );
  await expect(bracket).toBeVisible();
});

for (const [plugin, path, url] of [
  ['icpc', '/contests/1/rankings', `${ICPC}/standings`],
  ['ioi', '/contests/4/rankings', `${IOI}/scoreboard`],
] as const) {
  test(`an ${plugin.toUpperCase()} scoreboard opened before the start shows the contest once it starts`, async ({
    page,
  }) => {
    await open(page, plugin);
    let started = false;
    await page.route(url, (route) => {
      if (started) return route.fallback();
      const path = new URL(route.request().url()).pathname.replace(
        /^\/api\/v1/,
        '',
      );
      const live = PLUGIN_ROUTES.find(([, pattern]) => pattern.test(path));
      const board = live?.[2](new URL(route.request().url()), 'contestant', []);
      return route.fulfill({
        json: { ...(board as object), phase: 'before', rows: [], rankings: [] },
      });
    });
    await page.goto(path);
    await expect(page.getByText(/not started/i).first()).toBeVisible();

    started = true;
    await runClockUntil(page, () =>
      expect(page.getByRole('cell', { name: 'Alice' })).toBeVisible({
        timeout: 500,
      }),
    );
  });
}
