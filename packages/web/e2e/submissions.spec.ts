import { expect, test } from '@playwright/test';

import { cursorPage, mockApi, submission } from './api';

// A contestant's list of contest submissions names who submitted each row
// once it holds other people's rows too; with only their own, it does not.
test('the contest submissions list names the submitter of peer rows', async ({
  page,
}) => {
  await mockApi(page, 'contestant');
  await page.route('**/api/v1/contests/1/submissions?**', (route) =>
    route.fulfill({
      json: cursorPage([
        {
          ...submission(5, 3, 'Accepted'),
          username: 'bob',
          display_name: 'Bob',
        },
        submission(4, 2, 'WrongAnswer'),
      ]),
    }),
  );
  await page.goto('/contests/1/submissions');
  await expect(page.getByRole('columnheader', { name: 'User' })).toBeVisible();
  await expect(page.getByRole('cell', { name: /Bob/ })).toBeVisible();
});

test('the contest submissions list of only their own rows names nobody', async ({
  page,
}) => {
  await mockApi(page, 'contestant');
  await page.goto('/contests/1/submissions');
  await expect(page.getByRole('row').nth(1)).toBeVisible();
  await expect(page.getByRole('columnheader', { name: 'User' })).toHaveCount(0);
});
