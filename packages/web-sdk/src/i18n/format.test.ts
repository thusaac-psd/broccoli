import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  formatDateTime,
  formatRelativeSeconds,
  formatRelativeTime,
} from './format.ts';

// Absolute times are shown in the machine's zone; pin it.
process.env.TZ = 'UTC';

const AT = Date.UTC(2026, 10, 14, 0, 9);

test('date-times follow the UI locale, not a hand-built pattern', () => {
  const zh = formatDateTime(AT, 'zh-CN');
  assert.match(zh, /2026年11月14日/);
  assert.match(zh, /00:09/);
  const en = formatDateTime(AT, 'en');
  assert.match(en, /Nov 14, 2026/);
});

test('a date-time can name its time zone', () => {
  assert.match(formatDateTime(AT, 'en', { timeZoneName: true }), /(UTC|GMT)/);
});

test('relative times use the locale plural rules and say now for a fresh one', () => {
  assert.equal(formatRelativeSeconds(-86_400, 'en'), 'yesterday');
  assert.equal(formatRelativeSeconds(-3 * 86_400, 'en'), '3 days ago');
  assert.equal(formatRelativeSeconds(-60, 'en'), '1 minute ago');
  assert.equal(formatRelativeSeconds(2 * 3_600, 'en'), 'in 2 hours');
  assert.equal(formatRelativeSeconds(-5, 'en'), 'now');
  assert.equal(formatRelativeSeconds(-5, 'zh-CN'), '现在');
  assert.equal(formatRelativeSeconds(-180, 'zh-CN'), '3分钟前');
});

test('relative time is measured against the clock it is given', () => {
  assert.equal(
    formatRelativeTime(new Date(AT).toISOString(), AT + 7_200_000, 'en'),
    '2 hours ago',
  );
});
