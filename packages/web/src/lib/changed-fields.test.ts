import assert from 'node:assert/strict';
import { test } from 'node:test';

import { changedFields } from './changed-fields.ts';

test('changedFields keeps only the fields that differ', () => {
  assert.deepEqual(
    changedFields(
      { title: 'A', limit: 1, tags: ['x'], note: null },
      { title: 'A', limit: 2, tags: ['x', 'y'], note: null },
    ),
    { limit: 2, tags: ['x', 'y'] },
  );
});

test('changedFields is empty when nothing changed', () => {
  assert.deepEqual(changedFields({ a: { b: 1 } }, { a: { b: 1 } }), {});
});

test('changedFields reports a field cleared to null', () => {
  assert.deepEqual(changedFields({ end: '2026' }, { end: null }), {
    end: null,
  });
});
