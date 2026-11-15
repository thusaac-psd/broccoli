import { expect, test } from '@playwright/test';

import { cursorPage, mockApi, NOW, problem, submission } from './api';

test('a contest submission names its author by name and username, with no ids', async ({
  page,
}) => {
  await mockApi(page, 'admin');
  await page.goto('/contests/1/submissions');
  const author = page.getByRole('cell', { name: /Alice/ }).first();
  await expect(author).toHaveText('Alicealice');
  await expect(page.getByText(/contest #/)).toHaveCount(0);
});

test('a long problem title stays on one line on a phone, in full in its title', async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await mockApi(page, 'admin');
  await page.goto('/contests/1/submissions');
  const title = page
    .getByText('Shortest Path on a Very Long Named Grid Problem')
    .first();
  await expect(title).toHaveAttribute(
    'title',
    'Shortest Path on a Very Long Named Grid Problem',
  );
  const { height, lineHeight } = await title.evaluate((element) => ({
    height: element.getBoundingClientRect().height,
    lineHeight: parseFloat(getComputedStyle(element).lineHeight),
  }));
  expect(height).toBeLessThanOrEqual(lineHeight + 1);
});

const DISABLED_OUTAGE = { state: 'disabled', plugin_id: 'icpc' };
const WAITING = 'Waiting: its contest plugin is disabled';

/** A submission parked because its contest type's provider is disabled. */
const parked = (id: number) => ({
  ...submission(id, 2, 'Accepted'),
  status: 'Queued',
  verdict: null,
  score: null,
  time_used: null,
  memory_used: null,
  plugin_outage: DISABLED_OUTAGE,
});

test('a submission parked while its contest plugin is disabled says it is waiting, not queued', async ({
  page,
}) => {
  await mockApi(page, 'contestant');
  await page.route(/\/api\/v1\/contests\/1\/submissions(\?|$)/, (route) =>
    route.fulfill({ json: cursorPage([parked(6)]) }),
  );
  await page.goto('/contests/1/submissions');
  await expect(page.getByText(WAITING)).toBeVisible();
  await expect(page.getByText('Queued', { exact: true })).toHaveCount(0);
});

test('a parked submission says it is waiting and is asked for ever more slowly', async ({
  page,
}) => {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant');
  let polls = 0;
  await page.route(/\/api\/v1\/submissions\/6(\?|$)/, (route) => {
    polls += 1;
    return route.fulfill({
      json: { ...parked(6), files: [], result: null },
    });
  });
  await page.route('**/api/v1/submissions/6/judgements', (route) =>
    route.fulfill({ json: [] }),
  );
  await page.goto('/submissions/6');
  await expect(page.getByText(WAITING)).toBeVisible();

  await page.clock.runFor(120_000);
  await expect(page.getByText(WAITING)).toBeVisible();
  // At its usual second apart, two minutes would have been 120 polls.
  expect(polls).toBeLessThan(15);
});

test('a problem whose practice contest type has no provider offers no submit', async ({
  page,
}) => {
  await mockApi(page, 'contestant');
  await page.route(/\/api\/v1\/problems\/1(\?|$)/, (route) =>
    route.fulfill({
      json: { ...problem(1), plugin_outage: DISABLED_OUTAGE },
    }),
  );
  await page.goto('/problems/1?tab=coding');
  await expect(
    page.getByText(
      'Submitting is unavailable while the plugin that judges this problem is disabled.',
    ),
  ).toBeVisible();
  await expect(page.getByRole('button', { name: 'Submit' })).toHaveCount(0);
});
