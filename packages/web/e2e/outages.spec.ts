import { expect, type Page, test } from '@playwright/test';

import {
  contest,
  mockApi,
  NOW,
  problem,
  runClockUntil,
  submission,
} from './api';

// A plugin outage is temporary: what it closes or withholds is asked for
// again, slowly, and opens or shows again once the plugin is back.

const DISABLED_OUTAGE = { state: 'disabled', plugin_id: 'icpc' } as const;

const headers = { date: new Date(NOW).toUTCString() };

/** Counts the GETs of `path` the page sends, answered by `answer`. */
async function answerGets(
  page: Page,
  path: RegExp,
  answer: () => unknown,
): Promise<() => number> {
  let asked = 0;
  await page.route(path, (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    asked += 1;
    return route.fulfill({ headers, json: answer() });
  });
  return () => asked;
}

test('a submission an outage withholds is asked for slowly, and shows its result once the plugin is back', async ({
  page,
}) => {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant');
  let enabled = false;
  const base = {
    ...submission(4, 2, 'Accepted'),
    contest_id: 3,
    files: [{ filename: 'main.cpp', content: 'int main() {}\n' }],
  };
  const polls = await answerGets(page, /\/api\/v1\/submissions\/4(\?|$)/, () =>
    enabled
      ? {
          ...base,
          result: {
            verdict: 'Accepted',
            score: 100,
            time_used: 12,
            memory_used: 2048,
            compile_output: null,
            error_message: null,
            judged_at: new Date(NOW).toISOString(),
            judgement_id: 1,
            test_case_results: [],
            test_case_pagination: {
              page: 1,
              per_page: 20,
              total: 0,
              total_pages: 0,
            },
          },
        }
      : {
          ...base,
          status: null,
          verdict: null,
          score: null,
          result: null,
          withheld: { reason: 'plugin_outage', outage: DISABLED_OUTAGE },
        },
  );
  await page.route('**/api/v1/submissions/4/judgements', (route) =>
    route.fulfill({ headers, json: [] }),
  );
  await page.goto('/submissions/4');
  await expect(
    page
      .getByText("Result unavailable while the contest's plugin is disabled")
      .first(),
  ).toBeVisible();

  await page.clock.runFor(60_000);
  // At the outage pace, a minute is a couple of asks, not sixty.
  expect(polls()).toBeLessThan(6);

  enabled = true;
  await runClockUntil(page, () =>
    expect(page.getByText('Accepted').first()).toBeVisible({ timeout: 500 }),
  );
});

test('a contest problem closed by an outage opens for submitting once the plugin is back', async ({
  page,
}) => {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant');
  let enabled = false;
  const running = contest(3, 'Disabled Format Contest', -60, 240, 'running');
  await answerGets(page, /\/api\/v1\/contests\/3(\?|$)/, () =>
    enabled ? running : { ...running, plugin_outage: DISABLED_OUTAGE },
  );
  await page.route(/\/api\/v1\/contests\/3\/problems\/1(\?|$)/, (route) =>
    enabled ? route.fulfill({ headers, json: problem(1) }) : route.fallback(),
  );
  await page.route(/\/api\/v1\/contests\/3\/problems(\?|$)/, (route) =>
    enabled
      ? route.fulfill({
          headers,
          json: [
            {
              contest_id: 3,
              problem_id: 1,
              problem_title: 'Sum of Two Integers',
              label: 'A',
              position: 0,
            },
          ],
        })
      : route.fallback(),
  );
  await page.goto('/contests/3/problems/1?tab=coding');
  await expect(
    page
      .getByText(
        'Results for this contest are unavailable while its plugin is disabled.',
      )
      .first(),
  ).toBeVisible();
  await expect(page.getByRole('button', { name: 'Submit' })).toHaveCount(0);

  enabled = true;
  await runClockUntil(page, () =>
    expect(page.getByRole('button', { name: 'Submit' })).toBeVisible({
      timeout: 500,
    }),
  );
});

test('a practice problem closed by an outage opens for submitting once the plugin is back', async ({
  page,
}) => {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant');
  let enabled = false;
  await answerGets(page, /\/api\/v1\/problems\/1(\?|$)/, () =>
    enabled ? problem(1) : { ...problem(1), plugin_outage: DISABLED_OUTAGE },
  );
  await page.goto('/problems/1?tab=coding');
  await expect(page.getByRole('button', { name: 'Submit' })).toHaveCount(0);

  enabled = true;
  await runClockUntil(page, () =>
    expect(page.getByRole('button', { name: 'Submit' })).toBeVisible({
      timeout: 500,
    }),
  );
});

// A parked submission names what parks it, as the server records it.
for (const [provides, label] of [
  ['contest_type', 'Waiting: its contest plugin is disabled'],
  ['problem_type', 'Waiting: its evaluator plugin is disabled'],
] as const) {
  test(`a submission parked by its ${provides} plugin says so`, async ({
    page,
  }) => {
    await mockApi(page, 'contestant');
    await answerGets(page, /\/api\/v1\/submissions\/4(\?|$)/, () => ({
      ...submission(4, 2, 'Accepted'),
      status: 'Queued',
      verdict: null,
      score: null,
      files: [{ filename: 'main.cpp', content: 'int main() {}\n' }],
      result: null,
      parked_by: { plugin_id: 'some-plugin', provides },
    }));
    await page.route('**/api/v1/submissions/4/judgements', (route) =>
      route.fulfill({ headers, json: [] }),
    );
    await page.goto('/submissions/4');
    await expect(page.getByText(label).first()).toBeVisible();
  });
}
