import { expect, test } from '@playwright/test';

import {
  cursorPage,
  mockApi,
  NOW,
  problem,
  submission,
  SYSTEM_ERROR_DETAIL,
} from './api';

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
/** What the server says closes work while `icpc` is disabled. */
const CLOSED_BY_ICPC = { plugin_id: 'icpc', provides: 'contest_type' };
const WAITING = 'Waiting: its contest plugin is disabled';

/** A submission parked because its contest type's provider is disabled. */
const parked = (id: number) => ({
  ...submission(id, 2, 'Accepted'),
  status: 'Queued',
  verdict: null,
  score: null,
  time_used: null,
  memory_used: null,
  parked_by: CLOSED_BY_ICPC,
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

test('a parked submission shows one consistent status: waiting, with nothing in progress or spinning', async ({
  page,
}) => {
  await mockApi(page, 'contestant');
  await page.route(/\/api\/v1\/submissions\/6(\?|$)/, (route) =>
    route.fulfill({ json: { ...parked(6), files: [], result: null } }),
  );
  await page.route('**/api/v1/submissions/6/judgements', (route) =>
    route.fulfill({ json: [] }),
  );
  await page.goto('/submissions/6');
  await expect(page.getByText(WAITING)).toBeVisible();
  await expect(page.getByText('In progress', { exact: true })).toHaveCount(0);
  await expect(page.getByText('Judging...')).toHaveCount(0);
  await expect(page.locator('.animate-spin')).toHaveCount(0);
});

test('a problem whose practice contest type has no provider offers no submit', async ({
  page,
}) => {
  await mockApi(page, 'contestant');
  await page.route(/\/api\/v1\/problems\/1(\?|$)/, (route) =>
    route.fulfill({
      json: {
        ...problem(1),
        plugin_outage: DISABLED_OUTAGE,
        closed_by: CLOSED_BY_ICPC,
      },
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

const JUDGED_WITH_ISSUES = "Judged while this problem's settings had issues";

/** The current judgement of submission 1 (problem 2), with its issue flag. */
async function judgementOne(
  page: import('@playwright/test').Page,
  configIssues: boolean | null,
) {
  await page.route('**/api/v1/submissions/1/judgements/1?**', (route) =>
    route.fulfill({ json: { id: 1, config_issues: configIssues } }),
  );
}

test('staff see that a judgement ran while its problem had config issues, with the issues as they stand', async ({
  page,
}) => {
  await mockApi(page, 'admin');
  await judgementOne(page, true);
  await page.route('**/api/v1/problems/2/config/issues', (route) =>
    route.fulfill({
      json: [
        {
          plugin_id: 'ioi',
          scope: 'problem',
          ref_id: '2',
          namespace: 'task',
          errors: [
            {
              path: 'subtasks[0].test_cases[0]',
              message: "this problem has no test case 't2'",
              kind: 'plugin',
              key: null,
              params: {},
            },
          ],
        },
      ],
    }),
  );
  await page.goto('/submissions/1');
  const notice = page
    .getByRole('alert')
    .filter({ hasText: JUDGED_WITH_ISSUES });
  await expect(notice).toBeVisible();
  await expect(notice).toContainText('ioi/task on this problem');
});

test('the notice says a rejudge scores the submission once its problem has no config issues', async ({
  page,
}) => {
  await mockApi(page, 'admin');
  await judgementOne(page, true);
  await page.route('**/api/v1/problems/2/config/issues', (route) =>
    route.fulfill({ json: [] }),
  );
  await page.goto('/submissions/1');
  const notice = page
    .getByRole('alert')
    .filter({ hasText: JUDGED_WITH_ISSUES });
  await expect(notice).toContainText('Rejudge to score this submission');
});

test('a judgement whose problem had no config issues shows no notice', async ({
  page,
}) => {
  await mockApi(page, 'admin');
  await judgementOne(page, false);
  await page.goto('/submissions/1');
  await expect(
    page.getByRole('heading', { name: '#1', exact: true }),
  ).toBeVisible();
  await expect(page.getByText(JUDGED_WITH_ISSUES)).toHaveCount(0);
});

test('staff see a system error once, its cause in their language and the server message as detail', async ({
  page,
}) => {
  await page.addInitScript(() =>
    localStorage.setItem('broccoli-locale', 'zh-CN'),
  );
  await mockApi(page, 'admin');
  await page.goto('/submissions/5');
  await expect(page.getByText('系统消息', { exact: true })).toHaveCount(1);
  await expect(page.getByText(/队列/).first()).toBeVisible();
  await expect(page.getByText('服务器提供的详细信息')).toBeVisible();
  await expect(
    page.getByText('sandbox: cgroup memory controller unavailable'),
  ).toBeVisible();
});

test("a judge error a plugin raised shows in the language of that plugin's pack", async ({
  page,
}) => {
  await page.addInitScript(() =>
    localStorage.setItem('broccoli-locale', 'zh-CN'),
  );
  await mockApi(page, 'admin', { plugins: ['icpc'] });
  await page.route(/\/api\/v1\/submissions\/5$/, (route) =>
    route.fulfill({
      json: {
        ...SYSTEM_ERROR_DETAIL,
        result: {
          ...SYSTEM_ERROR_DETAIL.result,
          error_code: 'icpc.NO_TEST_CASES',
          error_message: 'Problem has no test cases; cannot be judged',
        },
      },
    }),
  );
  await page.goto('/submissions/5');
  await expect(
    page.getByText(
      '该题目没有测试点，无法评测。请联系比赛工作人员添加测试点。',
    ),
  ).toBeVisible();
});

test('an IOI submission that failed to judge shows the server message once, as the host labels it', async ({
  page,
}) => {
  await mockApi(page, 'contestant', { plugins: ['ioi'] });
  await page.route(
    '**/api/v1/p/ioi/api/plugins/ioi/contests/4/problems/3/config',
    (route) =>
      route.fulfill({
        json: {
          scoring_mode: 'sum_best_subtask',
          feedback_level: 'none',
          subtasks: [],
        },
      }),
  );
  await page.route('**/api/v1/p/ioi/api/plugins/ioi/contests/4/info', (route) =>
    route.fulfill({
      json: {
        scoring_mode: 'sum_best_subtask',
        feedback_level: 'none',
        scoreboard_visibility: 'all_contest_viewers',
        scoreboard_tiebreaker: 'equal_rank',
        token_mode: 'none',
      },
    }),
  );
  await page.route(/\/api\/v1\/submissions\/5$/, (route) =>
    route.fulfill({
      json: { ...SYSTEM_ERROR_DETAIL, contest_id: 4, contest_type: 'ioi' },
    }),
  );
  await page.goto('/contests/4/submissions/5');
  await expect(
    page.getByText('Results are not visible for this contest.'),
  ).toBeVisible();
  await expect(page.getByText('System Message', { exact: true })).toHaveCount(
    1,
  );
  await expect(page.getByText('Details from the server')).toBeVisible();
  await expect(
    page.getByText('sandbox: cgroup memory controller unavailable'),
  ).toHaveCount(1);
});
