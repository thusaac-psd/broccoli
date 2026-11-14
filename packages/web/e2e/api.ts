// A stand-in for the server, so the browser checks run with no backend and
// see the same data on every run. Every API call the app makes must have an
// answer here: an unanswered one is recorded and fails the test.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import type { Page, Route } from '@playwright/test';
import { parse } from 'smol-toml';

import plugin from './fixture-plugin.json' with { type: 'json' };
import registries from './fixture-registries.json' with { type: 'json' };

/** The instant every check runs at, in the browser and in the mock server. */
export const NOW = Date.parse('2026-06-01T10:00:00Z');

const at = (offsetMinutes: number) =>
  new Date(NOW + offsetMinutes * 60_000).toISOString();

const REPO = join(dirname(fileURLToPath(import.meta.url)), '../../..');

const ZH_CN = parse(
  readFileSync(join(REPO, 'plugins/broccoli-zh-cn/locales/zh-CN.toml'), 'utf8'),
) as Record<string, string>;

export type Viewer = 'guest' | 'admin' | 'contestant';

const ADMIN_PERMISSIONS = [
  'contest:create',
  'contest:delete',
  'contest:manage',
  'dlq:manage',
  'plugin:manage',
  'problem:create',
  'problem:delete',
  'problem:edit',
  'role:manage',
  'submission:rejudge',
  'submission:submit',
  'submission:view_all',
  'system:admin',
  'system:view',
  'user:manage',
];

const USERS = {
  admin: {
    id: 1,
    username: 'admin',
    display_name: 'Admin',
    roles: ['admin'],
    permissions: ADMIN_PERMISSIONS,
  },
  contestant: {
    id: 2,
    username: 'alice',
    display_name: 'Alice',
    roles: ['contestant'],
    permissions: ['submission:submit'],
  },
};

const contest = (
  id: number,
  title: string,
  startMinutes: number,
  endMinutes: number,
) => ({
  id,
  title,
  description: 'Practice round.',
  activate_time: at(-24 * 60),
  start_time: at(startMinutes),
  end_time: at(endMinutes),
  deactivate_time: null,
  is_public: true,
  submissions_visible: true,
  show_compile_output: true,
  show_participants_list: true,
  contest_type: 'icpc',
  created_at: at(-48 * 60),
  updated_at: at(-48 * 60),
});

const RUNNING = contest(1, 'Spring Training Contest 2026', -60, 240);
const UPCOMING = contest(
  2,
  'Upcoming Qualifier',
  2 * 24 * 60,
  2 * 24 * 60 + 180,
);

const PROBLEM_TITLES = [
  'Sum of Two Integers',
  'Shortest Path on a Very Long Named Grid Problem',
  'Matrix',
];

const problem = (id: number) => ({
  id,
  title: PROBLEM_TITLES[id - 1],
  content: '# Statement\n\nCompute $a+b$.\n\n```\n1 2\n```',
  time_limit: 1000,
  memory_limit: 262144,
  problem_type: 'batch',
  checker_format: 'exact',
  default_contest_type: 'icpc',
  show_test_details: false,
  is_public: true,
  submission_format: null,
  samples: [],
  created_at: at(-48 * 60),
  updated_at: at(-48 * 60),
});

const contestProblems = PROBLEM_TITLES.map((title, i) => ({
  contest_id: 1,
  problem_id: i + 1,
  problem_title: title,
  label: 'ABC'[i],
  position: i,
}));

const submission = (id: number, userId: number, verdict: string) => ({
  id,
  contest_id: 1,
  contest_type: 'icpc',
  problem_id: 1 + (id % 3),
  problem_title: PROBLEM_TITLES[id % 3],
  user_id: userId,
  username: userId === 1 ? 'admin' : 'alice',
  language: 'cpp',
  status: 'Judged',
  verdict,
  score: verdict === 'Accepted' ? 100 : 0,
  time_used: 12,
  memory_used: 2048,
  judge_epoch: 1,
  target_worker_id: null,
  created_at: at(-id),
});

const SUBMISSIONS = [
  submission(3, 2, 'Accepted'),
  submission(2, 2, 'WrongAnswer'),
  submission(1, 2, 'TimeLimitExceeded'),
];

const cursorPage = <T>(data: T[]) => ({
  data,
  pagination: {
    has_more: false,
    next_cursor: null,
    per_page: 20,
    total: null,
    total_pages: null,
  },
});

const numberedPage = <T>(data: T[]) => ({
  data,
  pagination: { page: 1, per_page: 25, total: data.length, total_pages: 1 },
});

const SUBMISSION_DETAIL = {
  ...submission(1, 2, 'Accepted'),
  files: [{ filename: 'main.cpp', content: 'int main() { return 0; }\n' }],
  result: {
    verdict: 'Accepted',
    score: 100,
    time_used: 12,
    memory_used: 2048,
    compile_output: null,
    error_message: null,
    judged_at: at(-1),
    judgement_id: 1,
    test_case_results: [],
    test_case_pagination: { page: 1, per_page: 20, total: 0, total_pages: 0 },
  },
};

type Handler = (url: URL, viewer: Viewer) => unknown;

/** `METHOD path-pattern` to the body answered. */
const ROUTES: [string, RegExp, Handler][] = [
  ['GET', /^\/i18n\/locales$/, () => ['en', 'zh-CN']],
  [
    'GET',
    /^\/i18n\/translations\/([\w-]+)$/,
    (url) => (url.pathname.endsWith('/zh-CN') ? ZH_CN : {}),
  ],
  ['GET', /^\/plugins\/active$/, () => []],
  ['GET', /^\/plugins\/registries$/, () => registries],
  [
    'GET',
    /^\/contests$/,
    (_, viewer) => numberedPage(viewer === 'guest' ? [] : [UPCOMING, RUNNING]),
  ],
  ['GET', /^\/contests\/1$/, () => RUNNING],
  ['GET', /^\/contests\/2$/, () => UPCOMING],
  [
    'GET',
    /^\/contests\/(\d+)\/me$/,
    (url) => ({
      contest_id: Number(url.pathname.split('/')[2]),
      user_id: 2,
      is_registered: true,
      registered_at: at(-24 * 60),
    }),
  ],
  ['GET', /^\/contests\/1\/problems$/, () => contestProblems],
  [
    'GET',
    /^\/contests\/1\/problems\/(\d+)$/,
    (url) => problem(Number(url.pathname.split('/')[4])),
  ],
  ['GET', /^\/contests\/1\/submissions$/, () => cursorPage(SUBMISSIONS)],
  ['GET', /^\/contests\/\d+\/clarifications$/, () => ({ data: [] })],
  ['GET', /^\/problems$/, () => numberedPage([3, 2, 1].map(problem))],
  [
    'GET',
    /^\/problems\/(\d+)$/,
    (url) => problem(Number(url.pathname.split('/')[2])),
  ],
  ['GET', /^\/submissions$/, () => cursorPage(SUBMISSIONS)],
  ['GET', /^\/submissions\/1$/, () => SUBMISSION_DETAIL],
  ['GET', /^\/submissions\/1\/judgements$/, () => []],
  [
    'GET',
    /^\/admin\/system\/overview$/,
    () => ({
      workers: [],
      queues: [
        {
          name: 'operation_tasks',
          depth: 0,
          breakdown: { unread: 0, pending: 0 },
        },
        {
          name: 'operation_tasks:reply:server-8f3a2c1d9e7b4a6f',
          depth: 0,
          breakdown: { unread: 0, pending: 0 },
        },
      ],
      submissions_in_progress: 0,
      dlq_unresolved_count: 0,
    }),
  ],
  ['GET', /^\/admin\/plugins$/, () => [plugin]],
  [
    'GET',
    /^\/admin\/plugins\/upload$/,
    () => ({ enabled: true, max_archive_bytes: 268435456 }),
  ],
  [
    'GET',
    /^\/users$/,
    () =>
      Object.values(USERS).map(({ id, username, display_name, roles }) => ({
        id,
        username,
        display_name,
        roles,
        created_at: at(-48 * 60),
      })),
  ],
  [
    'GET',
    /^\/dlq\/stats$/,
    () => ({
      total_unresolved: 0,
      total_resolved: 0,
      unresolved_by_message_type: {},
      unresolved_by_error_code: {},
    }),
  ],
  ['GET', /^\/dlq$/, () => numberedPage([])],
];

/**
 * Answers the app's API calls for `viewer` (signed in through the session
 * hint and the refresh cookie the mock pretends to hold). Returns the calls
 * nothing answered.
 */
export async function mockApi(page: Page, viewer: Viewer): Promise<string[]> {
  const unanswered: string[] = [];
  await page.clock.setFixedTime(NOW);
  if (viewer !== 'guest') {
    await page.addInitScript(
      (id) => localStorage.setItem('broccoli_session_user', String(id)),
      USERS[viewer].id,
    );
  }
  await page.route('**/api/v1/**', async (route: Route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname.replace(/^\/api\/v1/, '');
    const headers = { date: new Date(NOW).toUTCString() };
    if (request.method() === 'POST' && path === '/auth/refresh') {
      if (viewer === 'guest') {
        return route.fulfill({
          status: 401,
          headers,
          json: { code: 'TOKEN_MISSING', message: '' },
        });
      }
      return route.fulfill({
        headers,
        json: { token: 'e2e', ...USERS[viewer] },
      });
    }
    const match = ROUTES.find(
      ([method, pattern]) => method === request.method() && pattern.test(path),
    );
    if (!match) {
      unanswered.push(`${request.method()} ${path}`);
      return route.fulfill({
        status: 404,
        headers,
        json: { code: 'NOT_FOUND', message: path },
      });
    }
    return route.fulfill({ headers, json: match[2](url, viewer) });
  });
  return unanswered;
}
