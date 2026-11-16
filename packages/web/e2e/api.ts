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

const readToml = (path: string) =>
  parse(readFileSync(join(REPO, path), 'utf8')) as Record<string, unknown>;

const ZH_CN = readToml('plugins/broccoli-zh-cn/locales/zh-CN.toml') as Record<
  string,
  string
>;

interface PluginWeb {
  root: string;
  entry: string;
  css?: string[];
  components?: Record<string, string>;
  slots?: unknown[];
  routes?: unknown[];
}

/**
 * A plugin of this repository as the server serves it: its manifest's web
 * part in `/plugins/active`, its translations merged into the locale's, and
 * its built frontend (`pnpm build`) under `/assets/<id>/`.
 */
function repoPlugin(id: string) {
  const manifest = readToml(`plugins/${id}/plugin.toml`) as {
    name: string;
    web: PluginWeb;
    translations?: Record<string, string>;
  };
  const { web } = manifest;
  const translations = Object.fromEntries(
    Object.entries(manifest.translations ?? {}).map(([locale, file]) => [
      locale,
      readToml(`plugins/${id}/${file}`) as Record<string, string>,
    ]),
  );
  return {
    id,
    root: join(REPO, 'plugins', id, web.root),
    translations,
    active: {
      id,
      name: manifest.name,
      entry: `/assets/${id}/${web.entry}`,
      components: web.components ?? {},
      slots: web.slots ?? [],
      routes: web.routes ?? [],
      css: (web.css ?? []).map((file) => `/assets/${id}/${file}`),
    },
  };
}

type RepoPlugin = ReturnType<typeof repoPlugin>;

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

export const contest = (
  id: number,
  title: string,
  startMinutes: number,
  endMinutes: number,
  phase: 'upcoming' | 'running' | 'ended',
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
  phase,
});

const RUNNING = contest(1, 'Spring Training Contest 2026', -60, 240, 'running');
/** A running contest of the Codelink bracket format. */
const BRACKET_RUNNING = {
  ...contest(5, 'Bracket Final', -60, 240, 'running'),
  contest_type: 'codelink-bracket',
};
/** A running contest of the IOI format. */
const IOI_RUNNING = {
  ...contest(4, 'IOI Practice Round', -60, 240, 'running'),
  contest_type: 'ioi',
};
/** A running contest whose contest-type plugin an administrator disabled. */
const DISABLED = {
  ...contest(3, 'Disabled Format Contest', -60, 240, 'running'),
  plugin_outage: { state: 'disabled', plugin_id: 'icpc' },
};
const DISABLED_WITHHELD = {
  reason: 'plugin_outage',
  outage: DISABLED.plugin_outage,
};
const UPCOMING = contest(
  2,
  'Upcoming Qualifier',
  2 * 24 * 60,
  2 * 24 * 60 + 180,
  'upcoming',
);

const PROBLEM_TITLES = [
  'Sum of Two Integers',
  'Shortest Path on a Very Long Named Grid Problem',
  'Matrix',
];

export const problem = (id: number) => ({
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

export const submission = (id: number, userId: number, verdict: string) => ({
  id,
  contest_id: 1,
  contest_type: 'icpc',
  problem_id: 1 + (id % 3),
  problem_title: PROBLEM_TITLES[id % 3],
  user_id: userId,
  username: userId === 1 ? 'admin' : 'alice',
  display_name: userId === 1 ? 'Admin' : 'Alice',
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

/** A submission of the disabled contest: its outcome is withheld. */
const WITHHELD_SUBMISSION = {
  ...submission(4, 2, 'Accepted'),
  contest_id: 3,
  status: null,
  verdict: null,
  score: null,
  time_used: null,
  memory_used: null,
  withheld: DISABLED_WITHHELD,
};

const SUBMISSIONS = [
  submission(3, 2, 'Accepted'),
  submission(2, 2, 'WrongAnswer'),
  submission(1, 2, 'TimeLimitExceeded'),
];

export const cursorPage = <T>(data: T[]) => ({
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

/** A submission that failed to judge, with the operator's diagnostics. */
export const SYSTEM_ERROR_DETAIL = {
  ...submission(5, 2, 'SystemError'),
  status: 'SystemError',
  files: [{ filename: 'main.cpp', content: 'int main() { return 0; }\n' }],
  result: {
    ...SUBMISSION_DETAIL.result,
    verdict: 'SystemError',
    score: 0,
    error_code: 'OPERATION_LOST',
    error_message: 'sandbox: cgroup memory controller unavailable',
  },
};

/** An error answer, in the server's error shape. */
class ErrorReply {
  readonly status: number;
  readonly json: { code: string; message: string };
  constructor(status: number, json: { code: string; message: string }) {
    this.status = status;
    this.json = json;
  }
}

type Handler = (url: URL, viewer: Viewer, plugins: RepoPlugin[]) => unknown;

const PRINT_JOB = {
  id: 1,
  contest_id: 1,
  user_id: 2,
  username: 'alice',
  display_name: 'Alice',
  problem_label: 'A',
  submission_id: 3,
  language: 'cpp',
  filename: 'main.cpp',
  pages_est: 1,
  pages: null,
  location: 'Row 3',
  target_printer: null,
  status: 'pending',
  claimed_by: null,
  claimed_printer: null,
  error: null,
  created_at: Math.floor(NOW / 1000) - 60,
  claimed_at: null,
  printed_at: null,
};

/** Answers for the plugins of this repository that the browser suite serves. */
export const PLUGIN_ROUTES: [string, RegExp, Handler][] = [
  [
    'GET',
    /^\/p\/print\/api\/plugins\/print\/admin\/jobs$/,
    () => ({
      data: [PRINT_JOB],
      pagination: { page: 1, per_page: 50, total: 1, total_pages: 1 },
    }),
  ],
  [
    'GET',
    /^\/p\/print\/api\/plugins\/print\/admin\/stations$/,
    () => ({ data: [] }),
  ],
  [
    'GET',
    /^\/p\/icpc\/api\/plugins\/icpc\/contests\/1\/info$/,
    () => ({
      penalty_minutes: 20,
      count_compile_error: false,
      show_test_details: false,
    }),
  ],
  [
    'GET',
    /^\/p\/icpc\/api\/plugins\/icpc\/contests\/1\/standings$/,
    () => ({
      phase: 'during',
      penalty_minutes: 20,
      problem_labels: ['A', 'B', 'C'],
      full_board: true,
      rows: [
        {
          rank: 1,
          user_id: 2,
          username: 'alice',
          display_name: 'Alice',
          solved: 1,
          penalty: 23,
          problems: { A: { attempts: 2, solved: true, time: 3, penalty: 23 } },
        },
      ],
    }),
  ],
  [
    'GET',
    /^\/p\/ioi\/api\/plugins\/ioi\/contests\/4\/info$/,
    () => ({
      scoring_mode: 'sum_best_subtask',
      feedback_level: 'full',
      scoreboard_visibility: 'all_contest_viewers',
      scoreboard_tiebreaker: 'equal_rank',
      token_mode: 'none',
    }),
  ],
  [
    'GET',
    /^\/p\/ioi\/api\/plugins\/ioi\/contests\/4\/scoreboard$/,
    () => ({
      phase: 'during',
      scoring_mode: 'sum_best_subtask',
      feedback_level: 'full',
      scoreboard_visibility: 'all_contest_viewers',
      scoreboard_tiebreaker: 'equal_rank',
      max_scores: { '1': 100 },
      full_board: true,
      rankings: [
        {
          rank: 1,
          user_id: 2,
          username: 'alice',
          display_name: 'Alice',
          total_score: 40,
          total_time_seconds: 600,
          problems: [{ problem_id: 1, score: 40 }],
        },
      ],
    }),
  ],
  [
    'GET',
    /^\/p\/codelink-bracket\/api\/plugins\/codelink-bracket\/contests\/5\/bracket$/,
    () => ({ player_count: 0, matches: [] }),
  ],
];

/** `METHOD path-pattern` to the body answered. */
const ROUTES: [string, RegExp, Handler][] = [
  ['GET', /^\/i18n\/locales$/, () => ['en', 'zh-CN']],
  [
    'GET',
    /^\/i18n\/translations\/([\w-]+)$/,
    (url, _, plugins) => {
      const locale = url.pathname.split('/').at(-1) ?? 'en';
      return Object.assign(
        locale === 'zh-CN' ? { ...ZH_CN } : {},
        ...plugins.map((plugin) => plugin.translations[locale] ?? {}),
      );
    },
  ],
  [
    'GET',
    /^\/plugins\/active$/,
    (_, __, plugins) => plugins.map((plugin) => plugin.active),
  ],
  ['GET', /^\/plugins\/registries$/, () => registries],
  [
    'GET',
    /^\/contests$/,
    (_, viewer) => numberedPage(viewer === 'guest' ? [] : [UPCOMING, RUNNING]),
  ],
  // Reading a contest takes signing in.
  [
    'GET',
    /^\/contests\/1$/,
    (_, viewer) =>
      viewer === 'guest'
        ? new ErrorReply(401, { code: 'TOKEN_MISSING', message: '' })
        : RUNNING,
  ],
  ['GET', /^\/contests\/2$/, () => UPCOMING],
  ['GET', /^\/contests\/4$/, () => IOI_RUNNING],
  ['GET', /^\/contests\/5$/, () => BRACKET_RUNNING],
  ['GET', /^\/contests\/3$/, () => DISABLED],
  [
    'GET',
    /^\/contests\/3\/problems$/,
    () => [
      {
        contest_id: 3,
        problem_id: 1,
        problem_title: null,
        label: 'A',
        position: 0,
        withheld: DISABLED_WITHHELD,
      },
    ],
  ],
  [
    'GET',
    /^\/contests\/3\/problems\/1$/,
    () =>
      new ErrorReply(503, {
        code: 'PLUGIN_DISABLED',
        message: "The plugin 'icpc' is disabled",
      }),
  ],
  [
    'GET',
    /^\/contests\/3\/submissions$/,
    () => cursorPage([WITHHELD_SUBMISSION]),
  ],
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
  ['GET', /^\/submissions\/5$/, () => SYSTEM_ERROR_DETAIL],
  ['GET', /^\/submissions\/5\/judgements$/, () => []],
  [
    'GET',
    /^\/submissions\/\d+\/judgements\/(\d+)$/,
    (url) => ({
      id: Number(url.pathname.split('/')[4]),
      config_issues: false,
    }),
  ],
  [
    'GET',
    /^\/admin\/system\/overview$/,
    () => ({
      queue_state: 'reachable',
      workers_stale: false,
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
  [
    'GET',
    /^\/admin\/plugins$/,
    () => [plugin, { ...plugin, id: 'icpc', name: 'icpc', config_schemas: [] }],
  ],
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
        // Alice came from a bundle without her password.
        has_password: id !== USERS.contestant.id,
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

export interface MockOptions {
  /** Plugins of this repository to serve, with their built frontends. */
  plugins?: string[];
  /**
   * Answers for the plugins' own API calls, `METHOD path` to the body
   * (`PLUGIN_ROUTES` unless given).
   */
  pluginRoutes?: [string, RegExp, Handler][];
  /** Whether self-registration is open (default true). */
  registrationOpen?: boolean;
}

/**
 * Answers the app's API calls for `viewer` (signed in through the session
 * hint and the refresh cookie the mock pretends to hold), on a server whose
 * self-registration is open unless `registrationOpen` is false. Returns the
 * calls nothing answered.
 */
export async function mockApi(
  page: Page,
  viewer: Viewer,
  {
    plugins: ids = [],
    pluginRoutes = PLUGIN_ROUTES,
    registrationOpen = true,
  }: MockOptions = {},
): Promise<string[]> {
  const unanswered: string[] = [];
  const plugins = ids.map(repoPlugin);
  for (const plugin of plugins) {
    await page.route(`**/assets/${plugin.id}/**`, (route) => {
      const file = new URL(route.request().url()).pathname.slice(
        `/assets/${plugin.id}/`.length,
      );
      return route.fulfill({ path: join(plugin.root, file) });
    });
  }
  const routes = [...pluginRoutes, ...ROUTES];
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
    if (request.method() === 'GET' && path === '/auth/registration') {
      return route.fulfill({ headers, json: { open: registrationOpen } });
    }
    const match = routes.find(
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
    const body = match[2](url, viewer, plugins);
    if (body instanceof ErrorReply) {
      return route.fulfill({ status: body.status, headers, json: body.json });
    }
    return route.fulfill({ headers, json: body });
  });
  return unanswered;
}

/**
 * Moves the page's clock forward `stepMs` at a time until `expectation`
 * holds. A retry the page schedules runs on that clock, and its answer
 * arrives only after the step that fired it, so a single jump can leave the
 * page behind; stepping until the state shows lets the page catch up between
 * steps, whatever the machine's load.
 */
export async function runClockUntil(
  page: Page,
  expectation: () => Promise<void>,
  { stepMs = 16_000, steps = 30 }: { stepMs?: number; steps?: number } = {},
): Promise<void> {
  for (let step = 0; ; step += 1) {
    try {
      await expectation();
      return;
    } catch (error) {
      if (step >= steps) throw error;
    }
    await page.clock.runFor(stepMs);
  }
}
