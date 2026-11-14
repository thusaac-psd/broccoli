import { expect, test } from '@playwright/test';

import { mockApi } from './api';

const TABLE_PAGES = [
  '/admin',
  '/contests',
  '/problems',
  '/admin/users',
  '/admin/submissions',
  '/contests/1/submissions',
];

// On a phone a wide table scrolls sideways inside its frame; its text never
// breaks inside a word to fit, which in Chinese (no spaces) would stack a
// status or a date one character per line.
for (const path of TABLE_PAGES) {
  test(`${path} in Chinese keeps table text whole on a phone`, async ({
    page,
  }, info) => {
    test.skip(info.project.name !== 'phone', 'a phone-width check');
    await mockApi(page, 'admin');
    await page.addInitScript(() =>
      localStorage.setItem('broccoli-locale', 'zh-CN'),
    );
    await page.goto(path);
    await page.waitForLoadState('networkidle');
    await expect(page.locator('table').first()).toBeVisible();

    const broken = await page.evaluate(() => {
      const found: string[] = [];
      for (const cell of Array.from(document.querySelectorAll('th, td'))) {
        const walker = document.createTreeWalker(cell, NodeFilter.SHOW_TEXT);
        for (let node = walker.nextNode(); node; node = walker.nextNode()) {
          const text = node.textContent?.trim() ?? '';
          if (!text) continue;
          const range = document.createRange();
          range.selectNodeContents(node);
          const lines = new Set(
            Array.from(range.getClientRects()).map((rect) =>
              Math.round(rect.top),
            ),
          ).size;
          if (lines > text.split(/\s+/).length) found.push(text);
        }
      }
      return found;
    });
    expect(broken).toEqual([]);
  });
}
