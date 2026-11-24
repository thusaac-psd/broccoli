import { expect, test } from '@playwright/test';

import { mockApi } from './api';

const ISSUE = {
  plugin_id: 'ioi',
  scope: 'contest_problem',
  ref_id: '1:1',
  namespace: 'task',
  errors: [
    {
      path: 'subtasks[1].test_cases[0]',
      message: "this problem has no test case 't2'",
      kind: 'plugin',
      key: null,
      params: {},
    },
  ],
};

const TEST_CASE = {
  id: 7,
  label: 't2',
  position: 0,
  score: 50,
  is_sample: false,
  description: null,
  input_preview: '1 2',
  output_preview: '3',
  input_size: 3,
  output_size: 1,
  input_is_binary: false,
  expected_output_is_binary: false,
  created_at: '2025-10-01T00:00:00Z',
};

// Deleting a test case a stored config names is refused with the configs it
// would break; the admin sees them and can delete it anyway.
test('deleting a test case a config names lists the configs it breaks and deletes it once confirmed', async ({
  page,
}) => {
  await mockApi(page, 'admin');
  const deletes: string[] = [];
  await page.route('**/api/v1/problems/1/test-cases', (route) =>
    route.fulfill({ json: [TEST_CASE] }),
  );
  await page.route('**/api/v1/problems/1/config/issues', (route) =>
    route.fulfill({ json: [] }),
  );
  await page.route('**/api/v1/problems/1/test-cases/7?**', (route) => {
    const url = new URL(route.request().url());
    deletes.push(url.search);
    if (url.searchParams.get('force') === 'true') {
      return route.fulfill({ status: 204 });
    }
    return route.fulfill({
      status: 409,
      json: {
        code: 'CONFIG_WOULD_BREAK',
        message: 'The change would leave 1 stored config(s) refused',
        details: { issues: [ISSUE] },
      },
    });
  });
  await page.goto('/problems/1');
  await page.getByRole('button', { name: 'Edit' }).click();

  await page.getByRole('button', { name: 'Delete' }).last().click();
  await page
    .getByRole('dialog')
    .getByRole('button', { name: 'Delete' })
    .click();

  const prompt = page.getByRole('dialog', { name: 'Save this change anyway?' });
  await expect(prompt).toBeVisible();
  await expect(prompt.getByText('ioi/task in contest 1')).toBeVisible();
  await expect(prompt.getByText('subtasks[1].test_cases[0]')).toBeVisible();
  await prompt.getByRole('button', { name: 'Save anyway' }).click();

  await expect(prompt).toHaveCount(0);
  await expect(page.getByText('Test case deleted')).toBeVisible();
  expect(deletes).toEqual(['?force=false', '?force=true']);
});

// A contest's problems dialog lists what the plugins find wrong with the
// contest's configs and its problems' configs in it, naming each problem by
// its label in the contest.
test("the contest's problems dialog shows the contest's config issues", async ({
  page,
}) => {
  await mockApi(page, 'admin');
  await page.route('**/api/v1/contests/1/problems', (route) =>
    route.fulfill({
      json: [
        {
          contest_id: 1,
          problem_id: 1,
          label: 'B',
          position: 0,
          problem_title: 'A + B',
        },
      ],
    }),
  );
  await page.route('**/api/v1/contests/1/config/issues', (route) =>
    route.fulfill({
      json: [
        {
          plugin_id: 'codelink-qualifier',
          scope: 'contest',
          ref_id: '1',
          namespace: 'contest',
          errors: [
            {
              path: 'advancement_slots',
              message:
                'up to 4 contestants can qualify directly, more than the 2 advancement places',
              kind: 'plugin',
              key: null,
              params: {},
            },
          ],
        },
        { ...ISSUE, errors: [{ ...ISSUE.errors[0], path: 'subtasks' }] },
      ],
    }),
  );
  await page.goto('/contests');
  await page
    .getByRole('row', { name: /Spring Training Contest 2026/ })
    .getByRole('button', { name: 'Actions' })
    .click();
  await page.getByRole('menuitem', { name: 'Contest Problems' }).click();

  const alert = page.getByRole('dialog').getByRole('alert');
  await expect(alert).toContainText(
    'codelink-qualifier/contest on this contest',
  );
  await expect(alert).toContainText('advancement_slots');
  await expect(alert).toContainText('ioi/task for problem B');
});
