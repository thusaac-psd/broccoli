import assert from 'node:assert/strict';
import { test } from 'node:test';

import { playerLabel } from './player.ts';

test('playerLabel prefers the username', () => {
  assert.equal(playerLabel('alice', 10), 'alice');
});

test('playerLabel falls back to the user id when the name is missing', () => {
  assert.equal(playerLabel(null, 10), '#10');
});
