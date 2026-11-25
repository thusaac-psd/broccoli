import AxeBuilder from '@axe-core/playwright';
import { expect, type Page, test } from '@playwright/test';

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
const CLOSED_BY_ICPC = {
  plugin_id: 'icpc',
  provides: 'contest_type',
  reason: 'disabled',
};
const WAITING = 'Waiting: its contest plugin icpc is disabled';

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
      'Submitting is unavailable while icpc, the plugin that judges this problem, is disabled.',
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
          contest_id: null,
          problem_id: 2,
          label: null,
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

test("staff see in the Versions card that a version was applied after its problem's inputs changed", async ({
  page,
}) => {
  await mockApi(page, 'admin');
  const version = (id: number, earlier: boolean) => ({
    id,
    submission_id: 1,
    version: id,
    is_current: id === 2,
    is_finalized: true,
    status: 'Judged',
    verdict: 'Accepted',
    score: 100,
    judge_epoch: id,
    created_at: '2025-10-01T14:30:00Z',
    test_case_results: [],
    judged_on_earlier_inputs: earlier,
  });
  await page.route('**/api/v1/submissions/1/judgements', (route) =>
    route.fulfill({ json: [version(1, false), version(2, true)] }),
  );
  await page.goto('/submissions/1');
  const badges = page.getByText('Judged on earlier inputs', { exact: true });
  await expect(badges).toHaveCount(1);
  await expect(badges).toHaveAttribute(
    'title',
    /test data, limits, checker or other files changed/,
  );
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

test('a token unlocks a hidden IOI result where the problem page shows it, without a reload', async ({
  page,
}) => {
  await mockApi(page, 'contestant', { plugins: ['ioi'] });
  let tokened = false;
  const judged = {
    ...submission(9, 2, 'Accepted'),
    contest_id: 4,
    contest_type: 'ioi',
    problem_id: 1,
    problem_title: 'Sum of Two Integers',
  };
  // At feedback "none" the owner reads the outcome as withheld, status and
  // all, until a token unlocks it.
  const hidden = {
    ...judged,
    status: null,
    verdict: null,
    score: null,
    time_used: null,
    memory_used: null,
    withheld: { reason: 'hidden' },
  };
  const IOI = '**/api/v1/p/ioi/api/plugins/ioi/contests/4';
  await page.route(/\/api\/v1\/contests\/4\/problems$/, (route) =>
    route.fulfill({
      json: [
        {
          contest_id: 4,
          problem_id: 1,
          problem_title: 'Sum of Two Integers',
          label: 'A',
          position: 0,
        },
      ],
    }),
  );
  await page.route(/\/api\/v1\/contests\/4\/problems\/1$/, (route) =>
    route.fulfill({ json: { ...problem(1), default_contest_type: 'ioi' } }),
  );
  await page.route(/\/api\/v1\/contests\/4\/submissions(\?|$)/, (route) =>
    route.fulfill({ json: cursorPage([tokened ? judged : hidden]) }),
  );
  await page.route(`${IOI}/info`, (route) =>
    route.fulfill({
      json: {
        scoring_mode: 'sum_best_subtask',
        feedback_level: 'none',
        scoreboard_visibility: 'all_contest_viewers',
        scoreboard_tiebreaker: 'equal_rank',
        token_mode: 'fixed_budget',
      },
    }),
  );
  await page.route(`${IOI}/token-status`, (route) =>
    route.fulfill({
      json: {
        mode: 'fixed_budget',
        available: tokened ? 0 : 1,
        used: tokened ? 1 : 0,
        total: 1,
        next_regen_at: null,
        tokened_submission_ids: tokened ? [9] : [],
        usable_submissions: tokened
          ? []
          : [{ submission_id: 9, problem_id: 1 }],
      },
    }),
  );
  await page.route(`${IOI}/submissions/9/token`, (route) => {
    tokened = true;
    return route.fulfill({ json: { remaining_tokens: 0 } });
  });

  await page.goto('/contests/4/problems/1?tab=coding');
  await expect(page.getByText('Result hidden').first()).toBeVisible();
  await page
    .getByRole('button', { name: 'Use a token on submission #9' })
    .click();
  await page.getByRole('button', { name: 'Confirm' }).click();
  await expect(page.getByText('Result hidden')).toHaveCount(0);
  await expect(page.getByText('Accepted').first()).toBeVisible();
});

/** Whether the viewer spent its token on submission 9. */
type TokenState = { tokened: boolean };

/**
 * Serves `page` an IOI contest at feedback `subtask_scores` with one token
 * and submission 9, whose test case rows read redacted until `state` says
 * the token is spent on it.
 */
async function serveTokenAtSubtaskScores(page: Page, state: TokenState) {
  await mockApi(page, 'contestant', { plugins: ['ioi'] });
  const judged = {
    ...submission(9, 2, 'Accepted'),
    contest_id: 4,
    contest_type: 'ioi',
    problem_id: 1,
    problem_title: 'Sum of Two Integers',
  };
  const row = {
    id: 1,
    test_case_id: 101,
    label: 'a',
    verdict: 'Accepted',
    score: 1,
    time_used: 37,
    memory_used: 1024,
    is_sample: false,
  };
  const IOI = '**/api/v1/p/ioi/api/plugins/ioi/contests/4';
  await page.route(/\/api\/v1\/contests\/4\/problems$/, (route) =>
    route.fulfill({
      json: [
        {
          contest_id: 4,
          problem_id: 1,
          problem_title: 'Sum of Two Integers',
          label: 'A',
          position: 0,
        },
      ],
    }),
  );
  await page.route(/\/api\/v1\/contests\/4\/problems\/1$/, (route) =>
    route.fulfill({ json: { ...problem(1), default_contest_type: 'ioi' } }),
  );
  await page.route(/\/api\/v1\/contests\/4\/submissions(\?|$)/, (route) =>
    route.fulfill({ json: cursorPage([judged]) }),
  );
  // Test case rows are redacted at subtask scores until a token unlocks them.
  const rows = () =>
    state.tokened
      ? [row]
      : [{ ...row, verdict: null, score: null, time_used: null }];
  await page.route(/\/api\/v1\/submissions\/9(\?|$)/, (route) =>
    route.fulfill({
      json: {
        ...judged,
        files: [{ filename: 'main.cpp', content: 'int main() {}\n' }],
        result: {
          verdict: 'Accepted',
          score: 100,
          time_used: 500,
          memory_used: 1024,
          compile_output: null,
          error_message: null,
          judged_at: judged.created_at,
          judgement_id: 1,
          test_case_results: rows(),
          test_case_pagination: {
            page: 1,
            per_page: 20,
            total: 1,
            total_pages: 1,
          },
        },
      },
    }),
  );
  await page.route(/\/api\/v1\/submissions\/9\/judgements\/1(\?|$)/, (route) =>
    route.fulfill({
      json: {
        test_case_results: rows(),
        test_case_pagination: {
          page: 1,
          per_page: 20,
          total: 1,
          total_pages: 1,
        },
      },
    }),
  );
  await page.route(`${IOI}/info`, (route) =>
    route.fulfill({
      json: {
        scoring_mode: 'max_submission',
        feedback_level: 'subtask_scores',
        scoreboard_visibility: 'all_contest_viewers',
        scoreboard_tiebreaker: 'equal_rank',
        token_mode: 'fixed_budget',
      },
    }),
  );
  const subtask = { name: 'Small', scoring_method: 'sum', max_score: 100 };
  await page.route(`${IOI}/problems/1/config`, (route) =>
    route.fulfill({
      json: {
        scoring_mode: 'max_submission',
        feedback_level: 'subtask_scores',
        max_score: 100,
        subtasks: [{ ...subtask, test_cases: ['a'] }],
        label_map: { a: 101 },
        test_case_max_scores: { a: 1 },
      },
    }),
  );
  await page.route(`${IOI}/submissions/9/subtask-scores`, (route) =>
    route.fulfill({
      json: { subtasks: [{ ...subtask, score: 100 }], max_score: 100 },
    }),
  );
  await page.route(`${IOI}/token-status`, (route) =>
    route.fulfill({
      json: {
        mode: 'fixed_budget',
        available: state.tokened ? 0 : 1,
        used: state.tokened ? 1 : 0,
        total: 1,
        next_regen_at: null,
        tokened_submission_ids: state.tokened ? [9] : [],
        usable_submissions: state.tokened
          ? []
          : [{ submission_id: 9, problem_id: 1 }],
      },
    }),
  );
  await page.route(`${IOI}/submissions/9/token`, (route) => {
    state.tokened = true;
    return route.fulfill({ json: { remaining_tokens: 0 } });
  });
}

test('a token at subtask scores shows the test case rows of the submission it unlocks', async ({
  page,
}, info) => {
  test.skip(info.project.name === 'phone', 'the sidebar is a sheet there');
  const state = { tokened: false };
  await serveTokenAtSubtaskScores(page, state);
  // The submission's own page shows its test case rows redacted.
  await page.goto('/submissions/9');
  await expect(page.getByText('Test Case #1')).toBeVisible();
  await expect(page.getByText(/37\s?ms/)).toHaveCount(0);
  // Spend the token on the problem's page and come back without a reload.
  await page.getByRole('link', { name: 'View contest submissions' }).click();
  await page.getByRole('link', { name: 'A Sum of Two Integers' }).click();
  await page.getByRole('button', { name: 'Submit' }).click();
  await page
    .getByRole('button', { name: 'Use a token on submission #9' })
    .click();
  await page.getByRole('button', { name: 'Confirm' }).click();
  await expect(
    page.getByRole('button', { name: 'Use a token on submission #9' }),
  ).toHaveCount(0);
  // Back to the submission's page inside the app, as a link would go.
  await page.evaluate(() => {
    window.history.pushState(null, '', '/submissions/9');
    window.dispatchEvent(new PopStateEvent('popstate'));
  });
  await expect(page.getByText('Test Case #1')).toBeVisible();
  await expect(page.getByText(/37\s?ms/).first()).toBeVisible();
});

test('a token spent in one tab shows the result in another tab already open on it', async ({
  page,
}, info) => {
  test.skip(info.project.name === 'phone', 'one check of the tabs is enough');
  const state = { tokened: false };
  await serveTokenAtSubtaskScores(page, state);
  // The other tab shows the redacted rows and polls no more: the outcome
  // it may see is final.
  const other = await page.context().newPage();
  await serveTokenAtSubtaskScores(other, state);
  await other.goto('/submissions/9');
  await expect(other.getByText('Test Case #1')).toBeVisible();
  await expect(other.getByText(/37\s?ms/)).toHaveCount(0);

  await page.goto('/contests/4/problems/1?tab=coding');
  await page
    .getByRole('button', { name: 'Use a token on submission #9' })
    .click();
  await page.getByRole('button', { name: 'Confirm' }).click();
  await expect(other.getByText(/37\s?ms/).first()).toBeVisible();
});

for (const theme of ['light', 'dark']) {
  test(`a tokened IOI result reads clearly (${theme})`, async ({ page }) => {
    await serveTokenAtSubtaskScores(page, { tokened: true });
    await page.addInitScript((t) => localStorage.setItem('theme', t), theme);
    await page.goto('/contests/4/problems/1?tab=coding');
    await page.getByText('#9').first().click();
    // The whole result: the badge, the totals, the subtask and its rows.
    await expect(page.getByText('Tokened').first()).toBeVisible();
    await expect(page.getByText('Small')).toBeVisible();
    await expect(page.getByText(/37\s?ms/).first()).toBeVisible();

    const { violations } = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa'])
      .analyze();
    expect(
      violations.map((v) => ({
        rule: v.id,
        nodes: v.nodes.map((n) => n.target.join(' ')),
      })),
    ).toEqual([]);
  });
}
