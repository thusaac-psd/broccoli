import { expect, test } from '@playwright/test';

import { mockApi, NOW } from './api';

// A poll that fails keeps what the page already shows, says the view is not
// up to date, and asks again ever more slowly: thousands of seats asking a
// struggling server once a second would only keep it down.
test('a submission whose polls fail stays on screen and is asked for ever more slowly', async ({
  page,
}) => {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant');
  let polls = 0;
  await page.route('**/api/v1/submissions/1', (route) => {
    polls += 1;
    if (polls > 1) return route.fulfill({ status: 502, body: 'Bad Gateway' });
    return route.fulfill({
      json: {
        id: 1,
        contest_id: 1,
        contest_type: 'icpc',
        problem_id: 1,
        problem_title: 'Sum of Two Integers',
        user_id: 2,
        username: 'alice',
        language: 'cpp',
        status: 'Running',
        verdict: null,
        score: null,
        time_used: null,
        memory_used: null,
        judge_epoch: 1,
        target_worker_id: null,
        created_at: new Date(NOW).toISOString(),
        files: [{ filename: 'main.cpp', content: 'int main() {}\n' }],
        result: null,
      },
    });
  });
  await page.goto('/submissions/1');
  await expect(
    page.getByRole('heading', { name: '#1', exact: true }),
  ).toBeVisible();

  await page.clock.runFor(60_000);
  await expect(page.getByText('Could not refresh')).toBeVisible();
  await expect(
    page.getByRole('heading', { name: '#1', exact: true }),
  ).toBeVisible();
  // At a steady second apart, a minute would have been sixty polls.
  expect(polls).toBeLessThan(20);
});

test('a submission that cannot be loaded shows the failure, with a retry, instead of polling it', async ({
  page,
}) => {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant');
  let polls = 0;
  await page.route('**/api/v1/submissions/1', (route) => {
    polls += 1;
    return route.fulfill({ status: 502, body: 'Bad Gateway' });
  });
  await page.goto('/submissions/1');
  await page.clock.runFor(5_000);
  await expect(page.getByRole('button', { name: 'Retry' })).toBeVisible();
  const shown = polls;

  await page.clock.runFor(60_000);
  await expect(page.getByRole('button', { name: 'Retry' })).toBeVisible();
  expect(polls).toBe(shown);
});
