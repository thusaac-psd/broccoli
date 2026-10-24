import assert from 'node:assert/strict';
import { test } from 'node:test';

import { getUserDisplayName } from './user.ts';

test('display names take precedence without changing account identity', () => {
  const user = { username: 'oj_01', display_name: '  选手甲  ' };
  assert.equal(getUserDisplayName(user), '选手甲');
  assert.equal(user.username, 'oj_01');
  assert.equal(user.display_name, '  选手甲  ');
});

test('missing, null and blank display names fall back to the login name', () => {
  for (const display_name of [undefined, null, '', ' \t\n']) {
    assert.equal(
      getUserDisplayName({ username: 'oj_01', display_name }),
      'oj_01',
    );
  }
});

test('accounts with the same display name remain separate identities', () => {
  const users = [
    { username: 'oj_01', display_name: '同名选手' },
    { username: 'oj_02', display_name: '同名选手' },
  ];
  assert.deepEqual(users.map(getUserDisplayName), ['同名选手', '同名选手']);
  assert.deepEqual(
    users.map((user) => user.username),
    ['oj_01', 'oj_02'],
  );
});
