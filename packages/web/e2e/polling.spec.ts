import { expect, test } from '@playwright/test';

import { contest, mockApi, NOW, runClockUntil } from './api';

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

test('a submission opened while the server fails keeps being asked for, and shows once it answers', async ({
  page,
}) => {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant');
  let down = true;
  await page.route('**/api/v1/submissions/1', (route) =>
    down
      ? route.fulfill({ status: 502, body: 'Bad Gateway' })
      : route.fallback(),
  );
  await page.goto('/submissions/1');
  await runClockUntil(page, () =>
    expect(page.getByRole('button', { name: 'Retry' })).toBeVisible({
      timeout: 500,
    }),
  );

  down = false;
  await runClockUntil(page, () =>
    expect(page.getByRole('heading', { name: '#1', exact: true })).toBeVisible({
      timeout: 500,
    }),
  );
});

// Every view of a contest shares one query, which asks again once the
// server clock passes the phase change: one poll at a time, however many
// views are open and however slowly the server answers.
test('a contest whose start is due is asked for by one poller at a time, and shows it running', async ({
  page,
}) => {
  await page.clock.install({ time: NOW });
  await mockApi(page, 'contestant');
  const soon = contest(2, 'Upcoming Qualifier', 0.1, 180, 'upcoming');
  let asked = 0;
  await page.route(/\/api\/v1\/contests\/2(\?|$)/, async (route) => {
    asked += 1;
    const answer = asked;
    // A loaded server: answers take a second and a half, and the first ones
    // after the start still say upcoming.
    if (answer > 1) await new Promise((done) => setTimeout(done, 1_500));
    return route.fulfill({
      headers: { date: new Date(NOW).toUTCString() },
      json: { ...soon, phase: answer >= 4 ? 'running' : 'upcoming' },
    });
  });
  await page.goto('/contests/2');
  await expect(page.getByText('Starts In').first()).toBeVisible();

  await page.clock.runFor(20_000);
  // A timer per view refetching every second, each cancelling the one in
  // flight, sends dozens here.
  expect(asked).toBeLessThan(6);

  await runClockUntil(
    page,
    () =>
      expect(page.getByText('Ends In').first()).toBeVisible({ timeout: 500 }),
    { stepMs: 2_000 },
  );
  expect(asked).toBeLessThan(15);
});
