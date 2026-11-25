import { expect, type Page, test } from '@playwright/test';

import {
  contest,
  mockApi,
  NOW,
  problem,
  runClockUntil,
  submission,
} from './api';
import registries from './fixture-registries.json' with { type: 'json' };

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
      .getByText('Result unavailable while its contest plugin icpc is disabled')
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
        'Results for this contest are unavailable while its plugin icpc is disabled.',
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
    enabled
      ? problem(1)
      : {
          ...problem(1),
          plugin_outage: DISABLED_OUTAGE,
          closed_by: {
            plugin_id: 'icpc',
            provides: 'contest_type',
            reason: 'disabled',
          },
        },
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

// Every plugin a submission needs closes the submit the same way: the
// problem's evaluator as much as its contest type's plugin. The page says
// which one, once, and offers no Submit the server would refuse.
const EVALUATOR_CLOSED = {
  plugin_id: 'batch-evaluator',
  provides: 'problem_type',
  reason: 'disabled',
} as const;

for (const [name, path, api] of [
  [
    'a practice problem',
    '/problems/1?tab=coding',
    /\/api\/v1\/problems\/1(\?|$)/,
  ],
  [
    'a contest problem',
    '/contests/1/problems/1?tab=coding',
    /\/api\/v1\/contests\/1\/problems\/1(\?|$)/,
  ],
] as const) {
  test(`${name} whose evaluator is disabled says so and offers no Submit until it is back`, async ({
    page,
  }) => {
    await page.clock.install({ time: NOW });
    await mockApi(page, 'contestant');
    let enabled = false;
    await answerGets(page, api, () =>
      enabled ? problem(1) : { ...problem(1), closed_by: EVALUATOR_CLOSED },
    );
    await page.goto(path);
    await expect(
      page.getByText(
        'Submitting is unavailable while batch-evaluator, the plugin that evaluates this problem, is disabled.',
      ),
    ).toHaveCount(1);
    await expect(page.getByRole('button', { name: 'Submit' })).toHaveCount(0);

    enabled = true;
    await runClockUntil(page, () =>
      expect(page.getByRole('button', { name: 'Submit' })).toBeVisible({
        timeout: 500,
      }),
    );
  });
}

// A parked submission names what parks it, as the server records it: the
// plugin's role and the plugin itself, by its translated name where its pack
// gives one and by its id where none does.
for (const [name, locale, plugins, parkedBy, label] of [
  [
    'its contest type plugin, by its translated name',
    'zh-CN',
    ['icpc'],
    { plugin_id: 'icpc', provides: 'contest_type', reason: 'disabled' },
    '等待中：其比赛插件「ICPC 赛制」已被禁用',
  ],
  [
    'its evaluator plugin, by its id when no pack names it',
    'en',
    [],
    { plugin_id: 'some-plugin', provides: 'problem_type', reason: 'disabled' },
    'Waiting: its evaluator plugin some-plugin is disabled',
  ],
] as const) {
  test(`a submission parked by ${name} says so`, async ({ page }) => {
    await page.addInitScript(
      (l) => localStorage.setItem('broccoli-locale', l),
      locale,
    );
    await mockApi(page, 'contestant', { plugins: [...plugins] });
    await answerGets(page, /\/api\/v1\/submissions\/4(\?|$)/, () => ({
      ...submission(4, 2, 'Accepted'),
      status: 'Queued',
      verdict: null,
      score: null,
      files: [{ filename: 'main.cpp', content: 'int main() {}\n' }],
      result: null,
      parked_by: parkedBy,
    }));
    await page.route('**/api/v1/submissions/4/judgements', (route) =>
      route.fulfill({ headers, json: [] }),
    );
    await page.goto('/submissions/4');
    await expect(page.getByText(label).first()).toBeVisible();
  });
}

// A language plugin is a plugin a submission needs like any other: the
// problem says so before any code is written, naming the plugin, and never
// falls back to a plain-text editor whose submit the server refuses.
const LANGUAGES_CLOSED = {
  plugin_id: 'standard-languages',
  provides: 'language',
  reason: 'disabled',
} as const;

test('a problem whose every language is closed names the language plugin and offers no Submit', async ({
  page,
}) => {
  await mockApi(page, 'contestant');
  await answerGets(page, /\/api\/v1\/problems\/1(\?|$)/, () => ({
    ...problem(1),
    closed_by: LANGUAGES_CLOSED,
    closed_languages: [
      { language: 'c', closed_by: LANGUAGES_CLOSED },
      { language: 'cpp', closed_by: LANGUAGES_CLOSED },
    ],
  }));
  // The disabled plugin's languages are gone from the registries.
  await page.route(/\/api\/v1\/plugins\/registries$/, (route) =>
    route.fulfill({ headers, json: { ...registries, languages: [] } }),
  );
  await page.goto('/problems/1?tab=coding');
  await expect(
    page.getByText(
      "Submitting is unavailable while standard-languages, the plugin for this problem's language, is disabled.",
    ),
  ).toBeVisible();
  await expect(page.getByRole('button', { name: 'Submit' })).toHaveCount(0);
  await expect(page.getByText('Plain Text')).toHaveCount(0);
});

test('a problem with no language registered at all says so instead of a plain-text editor', async ({
  page,
}) => {
  await mockApi(page, 'contestant');
  await page.route(/\/api\/v1\/plugins\/registries$/, (route) =>
    route.fulfill({ headers, json: { ...registries, languages: [] } }),
  );
  await page.goto('/problems/1?tab=coding');
  await expect(
    page.getByText('No language is available to submit in right now.'),
  ).toBeVisible();
  await expect(page.getByRole('button', { name: 'Submit' })).toHaveCount(0);
  await expect(page.getByText('Plain Text')).toHaveCount(0);
});

test('a language whose plugin is disabled is listed as unavailable, with the plugin, while others stay open', async ({
  page,
}) => {
  await mockApi(page, 'contestant');
  const closed = {
    plugin_id: 'extra-languages',
    provides: 'language',
    reason: 'disabled',
  } as const;
  await answerGets(page, /\/api\/v1\/problems\/1(\?|$)/, () => ({
    ...problem(1),
    closed_languages: [{ language: 'java', closed_by: closed }],
  }));
  await page.goto('/problems/1?tab=coding');
  await expect(page.getByRole('button', { name: 'Submit' })).toBeVisible();
  await page
    .getByRole('button', { name: /^C\+\+|^C$/ })
    .first()
    .click();
  const item = page.getByRole('menuitem', {
    name: 'java: unavailable while extra-languages is disabled',
  });
  await expect(item).toBeVisible();
  await expect(item).toHaveAttribute('aria-disabled', 'true');
});

// Whether a problem takes code in a language is one reading, of the
// problem's `closed_by`/`closed_languages` and the registries, asked for
// again while anything is closed and at once when a submit is refused for a
// plugin: a language plugin enabled again opens the submit within an outage
// poll, and one disabled under an open page closes it on the refusal.
for (const [name, path, api, submitApi, code] of [
  [
    'a practice problem',
    '/problems/1?tab=coding',
    /\/api\/v1\/problems\/1(\?|$)/,
    /\/api\/v1\/problems\/1\/submissions$/,
    'PLUGIN_DISABLED',
  ],
  [
    'a contest problem',
    '/contests/1/problems/1?tab=coding',
    /\/api\/v1\/contests\/1\/problems\/1(\?|$)/,
    /\/api\/v1\/contests\/1\/problems\/1\/submissions$/,
    'PLUGIN_NOT_LOADED',
  ],
] as const) {
  const closedProblem = (reason: 'disabled' | 'not_loaded') => {
    const closedBy = { ...LANGUAGES_CLOSED, reason };
    return {
      ...problem(1),
      closed_by: closedBy,
      closed_languages: [
        { language: 'c', closed_by: closedBy },
        { language: 'cpp', closed_by: closedBy },
      ],
    };
  };

  test(`${name} whose language plugin is enabled again opens for submitting within an outage poll`, async ({
    page,
  }) => {
    await page.clock.install({ time: NOW });
    await mockApi(page, 'contestant');
    let enabled = false;
    await answerGets(page, api, () =>
      enabled ? problem(1) : closedProblem('disabled'),
    );
    await answerGets(page, /\/api\/v1\/plugins\/registries$/, () =>
      enabled ? registries : { ...registries, languages: [] },
    );
    await page.goto(path);
    await expect(page.getByText(/standard-languages/).first()).toBeVisible();
    await expect(page.getByRole('button', { name: 'Submit' })).toHaveCount(0);

    enabled = true;
    // Two outage polls at most, not the registries' cache lifetime.
    await runClockUntil(
      page,
      () =>
        expect(page.getByRole('button', { name: 'Submit' })).toBeVisible({
          timeout: 500,
        }),
      { steps: 4 },
    );
  });

  test(`${name} whose submit is refused for a disabled plugin closes its submit at once`, async ({
    page,
  }) => {
    await mockApi(page, 'contestant');
    let refused = false;
    await answerGets(page, api, () =>
      refused
        ? closedProblem(code === 'PLUGIN_DISABLED' ? 'disabled' : 'not_loaded')
        : problem(1),
    );
    await page.route(submitApi, (route) => {
      if (route.request().method() !== 'POST') return route.fallback();
      refused = true;
      return route.fulfill({
        status: 503,
        headers: { ...headers, 'retry-after': '30' },
        json: {
          code,
          message: 'refused',
          details: { plugin_id: 'standard-languages' },
        },
      });
    });
    await page.goto(path);
    await page.getByRole('button', { name: 'Submit' }).click();
    await expect(page.getByRole('button', { name: 'Submit' })).toHaveCount(0);
    await expect(
      page.getByRole('status').filter({ hasText: 'standard-languages' }),
    ).toBeVisible();
  });
}

test('a problem page asks for the registries once', async ({ page }) => {
  await mockApi(page, 'contestant');
  const asked = await answerGets(
    page,
    /\/api\/v1\/plugins\/registries$/,
    () => registries,
  );
  await page.goto('/problems/1?tab=coding');
  await expect(page.getByRole('button', { name: 'Submit' })).toBeVisible();
  expect(asked()).toBe(1);
});
