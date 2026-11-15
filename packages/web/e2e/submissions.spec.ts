import { expect, test } from '@playwright/test';

import { mockApi } from './api';

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
