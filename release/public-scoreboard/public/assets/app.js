import { resolveLanguage, translate } from './i18n.js';
import { rankStandings } from './ranking.js';
import { getUserDisplayName } from './user.js';

const byId = (id) => document.getElementById(id);
const make = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const icon = (name) => {
  const node = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  node.setAttribute('class', 'icon');
  node.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#i-${name}`);
  node.append(use);
  return node;
};
const describe = (node, label) => {
  node.title = label;
  node.setAttribute('aria-label', label);
  return node;
};
const time = (seconds) =>
  `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
let savedLanguage;
try {
  savedLanguage = localStorage.getItem('codelink-language');
} catch {
  // The board also works when browser storage is unavailable.
}
let language = resolveLanguage([
  new URL(location.href).searchParams.get('lang'),
  savedLanguage,
  ...navigator.languages,
]);
const t = (key, values) => translate(language, key, values);
const locale = () => (language === 'zh' ? 'zh-CN' : 'en-GB');
const clock = (value) =>
  new Intl.DateTimeFormat(locale(), {
    timeZone: 'Asia/Shanghai',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  }).format(new Date(value));
let mode = location.hash === '#accepted' ? 'accepted' : 'points';
let data;
let timer;
let loading = false;
let snapshotVersion;
let selectedProblem;
let errorKey;

function renderStatic() {
  document.documentElement.lang = language === 'zh' ? 'zh-CN' : 'en';
  document.title = `${t('contestTitle')} · ${t(mode === 'accepted' ? 'acceptedTab' : 'pointsTab')}`;
  for (const node of document.querySelectorAll('[data-i18n]'))
    node.textContent = t(node.dataset.i18n);
  for (const node of document.querySelectorAll('[data-i18n-label]'))
    node.setAttribute('aria-label', t(node.dataset.i18nLabel));
  for (const node of document.querySelectorAll('[data-i18n-title]'))
    node.title = t(node.dataset.i18nTitle);
  const toggle = describe(byId('language'), t('switchLanguage'));
  toggle.textContent = language === 'zh' ? 'EN' : '中';
  toggle.lang = language === 'zh' ? 'en' : 'zh-CN';
  byId('empty').textContent = t(data || errorKey ? 'empty' : 'loading');
  renderUpdated();
  renderError();
}

function renderUpdated() {
  if (!data) return;
  byId('updated').textContent = t('updated', { time: clock(data.updated_at) });
  byId('updated').title = `${new Intl.DateTimeFormat(locale(), {
    timeZone: 'Asia/Shanghai',
    dateStyle: 'medium',
    timeStyle: 'medium',
  }).format(new Date(data.updated_at))} (UTC+8)`;
}

function renderError() {
  byId('error').hidden = !errorKey;
  byId('error-text').textContent = errorKey
    ? t(errorKey, { time: data ? clock(data.updated_at) : '' })
    : '';
  if (!data && errorKey) byId('empty').textContent = t('empty');
}

function renderProblem() {
  const problem = data?.problems.find(
    (item) => item.problem_id === selectedProblem,
  );
  if (!problem) return;
  byId('problem-title').textContent = t('problemTitle', {
    label: problem.label,
  });
  byId('problem-meta').textContent = t('problemMeta', {
    ...problem,
    slots: data.slots_per_problem,
  });
  byId('problem-awards').replaceChildren(
    ...Array.from({ length: data.slots_per_problem }, (_, index) => {
      const award = problem.awards[index];
      const item = make('li');
      item.append(
        make('span', 'mono', String(index + 1)),
        make(
          'span',
          `award-name ${award ? '' : 'muted'}`,
          award ? getUserDisplayName(award) : t('emptySlot'),
        ),
        make('span', 'mono', award ? time(award.time_seconds) : '—'),
      );
      return item;
    }),
  );
}

function updateTableHint() {
  const wrap = byId('table-wrap');
  byId('table-hint').hidden =
    wrap.hidden || wrap.scrollWidth <= wrap.clientWidth;
}

function render(resetScroll = false) {
  renderStatic();
  document.body.dataset.mode = mode;
  for (const value of ['points', 'accepted'])
    byId(value).setAttribute('aria-pressed', String(mode === value));
  if (!data) return;
  const solved = mode === 'accepted';
  const wrap = byId('table-wrap');
  const previous = { left: wrap.scrollLeft, top: wrap.scrollTop };
  const focusedProblem = document.activeElement?.dataset.problemId;
  const advance = data.advancement;
  const phase = byId('phase');
  phase.dataset.phase = data.phase;
  phase.replaceChildren(
    make('span', 'phase-dot'),
    make('span', '', t(data.phase)),
  );
  const summary = [
    ['users', data.rows.length, t('players', { count: data.rows.length })],
    [
      'grid',
      data.problems.length,
      t('problems', { count: data.problems.length }),
    ],
    [
      'direct',
      data.qualified_count,
      t('qualifiedCount', { count: data.qualified_count }),
    ],
  ];
  if (advance)
    summary.push([
      'reserve',
      advance.reserve_user_ids.length,
      t('reserveCount', { count: advance.reserve_user_ids.length }),
    ]);
  if (data.pending_submissions)
    summary.push([
      'clock',
      data.pending_submissions,
      t('pendingCount', { count: data.pending_submissions }),
      'warning',
    ]);
  byId('summary').replaceChildren(
    ...summary.map(([symbol, value, label, style = '']) => {
      const metric = describe(make('span', `metric ${style}`), label);
      metric.append(icon(symbol), make('span', 'metric-value', String(value)));
      return metric;
    }),
  );
  byId('slots-section').hidden = solved;
  if (!solved)
    byId('slots').replaceChildren(
      ...data.problems.map((problem) => {
        const button = describe(
          make('button', 'slot'),
          t('slotDescription', { ...problem, slots: data.slots_per_problem }),
        );
        button.type = 'button';
        button.dataset.problemId = String(problem.problem_id);
        button.setAttribute('popovertarget', 'problem-detail');
        button.setAttribute('popovertargetaction', 'show');
        const pips = make('span', 'slot-pips');
        pips.setAttribute('aria-hidden', 'true');
        for (let slot = 0; slot < data.slots_per_problem; slot++) {
          pips.append(
            make('span', `slot-pip ${problem.awards[slot] ? 'filled' : ''}`),
          );
        }
        button.append(make('span', 'slot-label', problem.label), pips);
        button.addEventListener('click', () => {
          selectedProblem = problem.problem_id;
          renderProblem();
        });
        return button;
      }),
    );
  renderProblem();
  const headings = ['#', t('participant'), t(solved ? 'solved' : 'score')];
  if (!solved) headings.push(t('qualificationTime'));
  const head = make('tr');
  headings.forEach((label, index) => {
    const cell = make(
      'th',
      index === 0 ? 'rank' : index === 1 ? 'name' : '',
      label,
    );
    cell.scope = 'col';
    if (index === 0) cell.setAttribute('aria-label', t('rank'));
    if (index === 3) {
      cell.className = 'qualification-time';
      cell.prepend(icon('clock'));
      describe(cell, t('qualificationTimeHint'));
    }
    head.append(cell);
  });
  data.problems.forEach((problem) => {
    const cell = make('th', 'problem', problem.label);
    cell.scope = 'col';
    if (!solved) cell.append(make('small', '', `${problem.points}pt`));
    describe(cell, `${problem.label}${solved ? '' : ` · ${problem.points}pt`}`);
    head.append(cell);
  });
  byId('rankings').tHead.replaceChildren(head);
  const direct = new Set(advance?.direct_user_ids ?? []);
  const reserve = new Set(advance?.reserve_user_ids ?? []);
  const fragment = document.createDocumentFragment();
  for (const { row, rank } of rankStandings(data.rows, mode)) {
    const tr = make('tr');
    const name = make('th', 'name');
    name.scope = 'row';
    const identity = make('span', 'name-content');
    const displayName = make('span', 'display-name', getUserDisplayName(row));
    displayName.title = getUserDisplayName(row);
    identity.append(displayName);
    name.append(identity);
    tr.append(make('td', 'rank mono', String(rank)), name);
    const total = make(
      'td',
      'total mono',
      String(solved ? row.accepted : row.score),
    );
    if (!solved) describe(total, t('scoreDescription', { score: row.score }));
    tr.append(total);
    if (!solved) {
      const verdict = advance
        ? direct.has(row.user_id)
          ? 'qualified'
          : reserve.has(row.user_id)
            ? 'reserve'
            : 'not_advanced'
        : row.qualification_verdict;
      const statuses = {
        qualified: ['direct', t('qualified'), 'is-direct'],
        reserve: ['reserve', t('reserve'), 'is-reserve'],
        pending: ['clock', t('pending'), 'warning'],
      };
      if (statuses[verdict]) {
        const [symbol, label, style] = statuses[verdict];
        const status = describe(make('span', `qualification ${style}`), label);
        status.setAttribute('role', 'img');
        status.append(icon(symbol));
        identity.append(status);
      } else if (verdict === 'not_advanced') {
        identity.append(make('span', 'sr-only', ` · ${t('notAdvanced')}`));
      }
      tr.append(
        make(
          'td',
          'qualification-time mono',
          row.qualified_at_seconds === null
            ? '—'
            : time(row.qualified_at_seconds),
        ),
      );
    }
    for (const problem of data.problems) {
      const result = row.problems[String(problem.problem_id)];
      const cell = make(
        'td',
        result ? `result ${solved ? 'credited' : result.status}` : 'empty-cell',
      );
      if (result) {
        const label = t(
          solved
            ? 'cellAccepted'
            : {
                credited: 'cellScore',
                slots_full: 'legendFull',
                after_qualification: 'legendAfter',
              }[result.status],
          { points: problem.points, slot: result.slot },
        );
        describe(
          cell,
          `${problem.label} · ${label} · ${time(result.time_seconds)}`,
        );
        const mark = make('span', 'result-mark');
        mark.setAttribute('aria-hidden', 'true');
        mark.append(
          icon(!solved && result.status === 'slots_full' ? 'minus' : 'check'),
        );
        if (!solved && result.status === 'credited')
          mark.append(make('span', 'cell-score mono', `${problem.points}pt`));
        cell.append(mark, make('small', 'mono', time(result.time_seconds)));
      } else {
        cell.textContent = '·';
        cell.setAttribute('aria-label', `${problem.label} · ${t('cellEmpty')}`);
      }
      tr.append(cell);
    }
    fragment.append(tr);
  }
  byId('rankings').tBodies[0].replaceChildren(fragment);
  byId('empty').hidden = data.rows.length > 0;
  byId('empty').textContent = t('empty');
  wrap.hidden = data.rows.length === 0;
  wrap.scrollLeft = resetScroll ? 0 : previous.left;
  wrap.scrollTop = resetScroll ? 0 : previous.top;
  if (focusedProblem)
    byId('slots')
      .querySelector(`[data-problem-id="${focusedProblem}"]`)
      ?.focus({ preventScroll: true });
  updateTableHint();
}

function showError(key) {
  errorKey = key;
  renderError();
}

async function refresh() {
  if (loading) return;
  loading = true;
  clearTimeout(timer);
  byId('refresh').disabled = true;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 8000);
  try {
    const response = await fetch('/api/standings', {
      cache: 'no-store',
      credentials: 'omit',
      signal: controller.signal,
    });
    if (!response.ok) throw new Error('Unavailable');
    const next = await response.json();
    if (
      !Array.isArray(next.rows) ||
      !Array.isArray(next.problems) ||
      !Number.isFinite(Date.parse(next.updated_at))
    )
      throw new Error('Invalid snapshot');
    // Export timestamps change every poll; keep focus and open details stable
    // unless the actual standings changed.
    const version = JSON.stringify({ ...next, updated_at: undefined });
    data = next;
    if (version !== snapshotVersion) {
      render();
      snapshotVersion = version;
    }
    renderUpdated();
    showError(
      Date.now() - Date.parse(data.updated_at) > 20000 ? 'stale' : null,
    );
  } catch {
    showError(data ? 'updateFailed' : 'empty');
  } finally {
    clearTimeout(timeout);
    loading = false;
    byId('refresh').disabled = false;
    timer = setTimeout(refresh, 5000);
  }
}

for (const value of ['points', 'accepted'])
  byId(value).addEventListener('click', () => {
    mode = value;
    const url = new URL(location.href);
    url.hash = value === 'accepted' ? 'accepted' : '';
    history.replaceState(null, '', url);
    render(true);
  });
window.addEventListener('hashchange', () => {
  mode = location.hash === '#accepted' ? 'accepted' : 'points';
  render(true);
});
byId('refresh').addEventListener('click', refresh);
byId('language').addEventListener('click', () => {
  language = language === 'zh' ? 'en' : 'zh';
  try {
    localStorage.setItem('codelink-language', language);
  } catch {
    // Keep the selected language for this visit even without browser storage.
  }
  const url = new URL(location.href);
  url.searchParams.set('lang', language);
  history.replaceState(null, '', url);
  render();
});
new ResizeObserver(updateTableHint).observe(byId('table-wrap'));
render();
refresh();
