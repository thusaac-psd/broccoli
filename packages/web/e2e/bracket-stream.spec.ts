import { expect, test } from '@playwright/test';

import { cursorPage, mockApi, NOW, PLUGIN_ROUTES, submission } from './api';

// While a player's live bracket stream is connected, it alone keeps the
// bracket current: the server re-reads the bracket for it every second, so
// the page does not read the same route again over HTTP.

const BRACKET =
  /^\/p\/codelink-bracket\/api\/plugins\/codelink-bracket\/contests\/5\/bracket$/;

const MATCH =
  /^\/p\/codelink-bracket\/api\/plugins\/codelink-bracket\/contests\/5\/matches\/1$/;

/** Alice's (user 2) first-round match, waiting for both orders. */
const SNAPSHOT = {
  player_count: 2,
  matches: [
    {
      id: 1,
      round: 1,
      stage_count: 1,
      kind: 'elimination',
      third_place_enabled: false,
      stage_start_at_ms: null,
      ordering_deadline_ms: null,
      next_game_at_ms: null,
      pos: 0,
      player_a: 2,
      player_b: 3,
      player_a_name: 'alice',
      player_b_name: 'bob',
      group_a: [1, 2, 3],
      group_b: [4, 5, 6],
      order_a: null,
      order_b: null,
      tiebreak_problem: null,
      wins_to_advance: 2,
      score_a: 0,
      score_b: 0,
      state: 'ordering',
      winner: null,
      decided_at_ms: 0,
      awaiting_submission_id: null,
      adjudication_reason: null,
      current_xiaoju_index: null,
      current_xiaoju_deadline_ms: null,
      current_xiaoju_opened_at_ms: null,
      games: [],
      starts_at_ms: null,
    },
  ],
};

test('a connected bracket stream stops the bracket polls', async ({ page }) => {
  await page.clock.install({ time: NOW });
  // The stream: one snapshot, then held open.
  await page.addInitScript((snapshot) => {
    const fetch = window.fetch.bind(window);
    window.fetch = (input, init) => {
      const accept = new Headers(init?.headers).get('Accept');
      if (accept === 'text/event-stream') {
        const body = new ReadableStream<Uint8Array>({
          start(controller) {
            controller.enqueue(
              new TextEncoder().encode(
                `event: snapshot\ndata: ${JSON.stringify(snapshot)}\n\n`,
              ),
            );
          },
        });
        return Promise.resolve(
          new Response(body, {
            headers: { 'content-type': 'text/event-stream' },
          }),
        );
      }
      return fetch(input, init);
    };
  }, SNAPSHOT);
  let polls = 0;
  await mockApi(page, 'contestant', {
    plugins: ['codelink-bracket'],
    pluginRoutes: [
      [
        'GET',
        BRACKET,
        () => {
          polls += 1;
          return SNAPSHOT;
        },
      ],
      ...PLUGIN_ROUTES,
    ],
  });
  await page.goto('/contests/5');
  await expect(
    page.getByRole('heading', { name: 'Bracket Final' }),
  ).toBeVisible();
  await expect(page.getByRole('complementary')).toContainText('game reminders');
  await expect(page.getByText('Connecting to game reminders…')).toHaveCount(0);
  const before = polls;
  await page.clock.runFor(60_000);
  expect(polls - before).toBe(0);
});

test("a match's submission feed names outcomes and times in the UI language", async ({
  page,
}) => {
  await page.addInitScript(() =>
    localStorage.setItem('broccoli-locale', 'zh-CN'),
  );
  await mockApi(page, 'admin', {
    plugins: ['codelink-bracket'],
    pluginRoutes: [
      ['GET', BRACKET, () => SNAPSHOT],
      ['GET', MATCH, () => SNAPSHOT.matches[0]],
      ...PLUGIN_ROUTES,
    ],
  });
  await page.route(/\/api\/v1\/contests\/5\/submissions(\?|$)/, (route) =>
    route.fulfill({
      json: cursorPage([
        { ...submission(7, 2, 'WrongAnswer'), contest_id: 5, problem_id: 1 },
        {
          ...submission(8, 2, 'Accepted'),
          contest_id: 5,
          problem_id: 2,
          status: 'Queued',
          verdict: null,
          parked_by: {
            plugin_id: 'codelink-bracket',
            provides: 'contest_type',
          },
        },
      ]),
    }),
  );
  await page.goto('/contests/5/rankings');
  await page.getByText('alice').first().click();
  const sheet = page.getByRole('dialog');
  await expect(sheet.getByText('答案错误').first()).toBeVisible();
  await expect(
    sheet.getByText('等待中：其比赛插件 Codelink 淘汰赛 已被禁用').first(),
  ).toBeVisible();
  await expect(sheet.getByText('WrongAnswer')).toHaveCount(0);
  await expect(sheet.getByText('Queued', { exact: true })).toHaveCount(0);
  await expect(sheet.getByText(/[AP]M/)).toHaveCount(0);
});
