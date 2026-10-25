import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';

import { messages, resolveLanguage, translate } from './public/assets/i18n.js';

test('language choice respects URL, saved preference, then browser languages', () => {
  assert.equal(resolveLanguage(['en', 'zh', 'zh-CN']), 'en');
  assert.equal(resolveLanguage([null, 'zh', 'en-US']), 'zh');
  assert.equal(resolveLanguage([null, null, 'fr-FR', 'en-GB']), 'en');
  assert.equal(resolveLanguage(['ZH-cn']), 'zh');
  assert.equal(resolveLanguage(['constructor', 'fr-FR']), 'zh');
});

test('both languages cover the same messages and interpolation values', () => {
  assert.deepEqual(
    Object.keys(messages.zh).sort(),
    Object.keys(messages.en).sort(),
  );
  for (const key of Object.keys(messages.zh)) {
    const placeholders = (value) =>
      [...value.matchAll(/\{(\w+)\}/g)].map((match) => match[1]).sort();
    assert.ok(messages.en[key].trim(), key);
    assert.deepEqual(
      placeholders(messages.zh[key]),
      placeholders(messages.en[key]),
      key,
    );
  }
});

test('static text, tooltips, and accessible labels all have translations', async () => {
  const html = await readFile(
    new URL('./public/index.html', import.meta.url),
    'utf8',
  );
  for (const [, key] of html.matchAll(
    /data-i18n(?:-label|-title)?="([^"]+)"/g,
  )) {
    for (const language of ['zh', 'en'])
      assert.ok(messages[language][key], `${language}:${key}`);
  }
});

test('score descriptions distinguish weighted points from the slot number', () => {
  assert.equal(translate('zh', 'score'), '得分');
  assert.equal(translate('en', 'score'), 'Score');
  assert.equal(
    translate('en', 'cellScore', { points: 2, slot: 1 }),
    'Score: 2pt · Slot #1',
  );
  assert.equal(
    translate('zh', 'cellScore', { points: 1, slot: 2 }),
    '得分 1pt · 名额 #2',
  );
});
