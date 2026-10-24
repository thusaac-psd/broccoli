import assert from 'node:assert/strict';
import { test } from 'node:test';

import { playerLabel } from './player.ts';

test('playerLabel uses the resolved display name or login name', () => {
  assert.equal(playerLabel(' 选手甲 ', 10), '选手甲');
  assert.equal(playerLabel('oj_01', 10), 'oj_01');
});

test('playerLabel falls back to the user id when the name is missing', () => {
  for (const name of [null, undefined, '', ' \t\n']) {
    assert.equal(playerLabel(name, 10), '#10');
  }
});
