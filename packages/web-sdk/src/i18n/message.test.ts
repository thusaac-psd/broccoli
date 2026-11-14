import assert from 'node:assert/strict';
import { test } from 'node:test';

import { formatMessage, messageParameters } from './message.ts';

const DAYS = '{count, plural, one {# day ago} other {# days ago}}';

test('plural messages pick the branch the locale plural rules select', () => {
  assert.equal(formatMessage(DAYS, 'en', { count: 1 }), '1 day ago');
  assert.equal(formatMessage(DAYS, 'en', { count: 2 }), '2 days ago');
  assert.equal(formatMessage(DAYS, 'en', { count: 0 }), '0 days ago');
  assert.equal(formatMessage(DAYS, 'en', { count: '1' }), '1 day ago');
});

test('an exact branch wins over the plural category', () => {
  const files = '{n, plural, =0 {No files} one {# file} other {# files}}';
  assert.equal(formatMessage(files, 'en', { n: 0 }), 'No files');
  assert.equal(formatMessage(files, 'en', { n: 1 }), '1 file');
  assert.equal(formatMessage(files, 'en', { n: 1200 }), '1,200 files');
});

test('locales without a singular fall back to other', () => {
  const zh = '{count, plural, other {# 个文件}}';
  assert.equal(formatMessage(zh, 'zh-CN', { count: 1 }), '1 个文件');
});

test('branches interpolate other parameters', () => {
  const text =
    'Created {count, plural, one {# user} other {# users}} in {group}';
  assert.equal(
    formatMessage(text, 'en', { count: 1, group: 'A' }),
    'Created 1 user in A',
  );
});

test('simple placeholders accept single and double braces', () => {
  assert.equal(
    formatMessage('{a} and {{b}}', 'en', { a: 1, b: 'x' }),
    '1 and x',
  );
});

test('a placeholder without its parameter stays as written', () => {
  assert.equal(formatMessage('Hi {name}', 'en', {}), 'Hi {name}');
  assert.equal(formatMessage(DAYS, 'en', {}), DAYS);
  assert.equal(formatMessage('Hi {name}', 'en'), 'Hi {name}');
});

test('unbalanced braces are left alone', () => {
  assert.equal(formatMessage('a { b', 'en', { b: 1 }), 'a { b');
});

test('message parameters include plural arguments and their branches', () => {
  assert.deepEqual(messageParameters(DAYS), ['count']);
  assert.deepEqual(
    messageParameters('{n, plural, one {# in {g}} other {# in {g}}} by {who}'),
    ['g', 'n', 'who'],
  );
  assert.deepEqual(messageParameters('{{count}} test files'), ['count']);
  assert.deepEqual(messageParameters('{count} 天前'), ['count']);
});
