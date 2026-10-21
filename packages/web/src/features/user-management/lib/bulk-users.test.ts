import assert from 'node:assert/strict';
import test from 'node:test';

import { normalizeBulkUsers } from './bulk-users.ts';

test('normalizes usernames while preserving supplied passwords exactly', () => {
  assert.deepEqual(
    normalizeBulkUsers([
      ' alice ',
      { username: 'bob', password: ' supplied_pass ' },
      { username: 'carol', password: null },
    ]),
    [
      { username: 'alice', display_name: 'alice', password: undefined },
      { username: 'bob', display_name: 'bob', password: ' supplied_pass ' },
      { username: 'carol', display_name: 'carol', password: undefined },
    ],
  );
});

test('preserves independent Unicode names and defaults blank names for legacy imports', () => {
  const users = normalizeBulkUsers([
    { username: 'a', display_name: ' 第一队 🌱 ' },
    { username: 'b', display_name: '第一队 🌱' },
    { username: 'c', display_name: '  ' },
    { username: 'd', display_name: null },
    { username: 'e', display_name: '🌱'.repeat(64) },
  ]);
  assert.deepEqual(
    users.map((u) => u.display_name),
    ['第一队 🌱', '第一队 🌱', 'c', 'd', '🌱'.repeat(64)],
  );
});

test('rejects malformed display names before sending an import', () => {
  for (const display_name of [
    123,
    false,
    {},
    '张'.repeat(65),
    'bad\nname',
    'bad\0name',
  ]) {
    assert.throws(
      () => normalizeBulkUsers([{ username: 'a', display_name }]),
      /InvalidDisplayName/,
    );
  }
});

test('rejects invalid and duplicate usernames before an import', () => {
  for (const input of [
    [],
    {},
    ['valid', 'bad-name'],
    ['duplicate', ' Duplicate '],
    [{ username: 'x'.repeat(33) }],
    [''],
  ]) {
    assert.throws(() => normalizeBulkUsers(input));
  }
});

test('explicit invalid passwords never become generated passwords', () => {
  for (const password of ['', 'short', 123, false, 'é'.repeat(65)]) {
    assert.throws(
      () => normalizeBulkUsers([{ username: 'alice', password }]),
      /admin.bulkParticipantsInvalidPassword/,
    );
  }
  assert.equal(
    normalizeBulkUsers([{ username: 'alice', password: 'é'.repeat(64) }])[0]
      .password,
    'é'.repeat(64),
  );
});
