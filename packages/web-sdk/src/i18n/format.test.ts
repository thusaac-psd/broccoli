import assert from 'node:assert/strict';
import { test } from 'node:test';

import { formatRelativeSeconds } from './format.ts';

test('relative times use the locale plural rules and say now for a fresh one', () => {
  assert.equal(formatRelativeSeconds(-86_400, 'en'), 'yesterday');
  assert.equal(formatRelativeSeconds(-3 * 86_400, 'en'), '3 days ago');
  assert.equal(formatRelativeSeconds(-60, 'en'), '1 minute ago');
  assert.equal(formatRelativeSeconds(2 * 3_600, 'en'), 'in 2 hours');
  assert.equal(formatRelativeSeconds(-5, 'en'), 'now');
  assert.equal(formatRelativeSeconds(-5, 'zh-CN'), '现在');
  assert.equal(formatRelativeSeconds(-180, 'zh-CN'), '3分钟前');
});

test('a timestamp a moment ahead of the clock reads as now, never as negative', () => {
  assert.equal(formatRelativeSeconds(1, 'en'), 'now');
  assert.equal(formatRelativeSeconds(1, 'zh-CN'), '现在');
});
