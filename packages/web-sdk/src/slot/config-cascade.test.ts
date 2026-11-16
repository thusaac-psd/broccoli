import assert from 'node:assert/strict';
import { test } from 'node:test';

import { deepMerge, resolveInheritedValue } from './config-cascade.ts';

test('deepMerge merges objects recursively and replaces arrays', () => {
  assert.deepEqual(
    deepMerge(
      { a: 1, nested: { x: 1, y: 2 }, list: [1, 2] },
      { nested: { y: 3 }, list: [9] },
    ),
    { a: 1, nested: { x: 1, y: 3 }, list: [9] },
  );
});

test('a field the contest does not store is inherited from the problem', () => {
  const inherited = {
    contest: { values: { python3: { interpreter: 'pypy3' } }, enabled: true },
    problem: {
      values: { cpp: { compiler: 'gcc-14' }, python3: { flags: ['-O'] } },
      enabled: true,
    },
  };
  assert.deepEqual(resolveInheritedValue('cpp', inherited), {
    value: { compiler: 'gcc-14' },
    source: 'Problem',
  });
  assert.deepEqual(resolveInheritedValue('python3', inherited), {
    value: { flags: ['-O'], interpreter: 'pypy3' },
    source: 'Contest',
  });
  assert.equal(resolveInheritedValue('java', inherited), null);
});

test('the most specific scope that says decides whether anything is inherited', () => {
  const off = { values: { limit: 1 }, enabled: false };
  const on = { values: { limit: 2 }, enabled: true };
  const unsaid = { values: { other: 3 }, enabled: null };
  assert.equal(
    resolveInheritedValue('limit', { contest: off, problem: on }),
    null,
  );
  assert.deepEqual(
    resolveInheritedValue('limit', { contest: on, problem: off }),
    { value: 2, source: 'Contest' },
  );
  assert.equal(
    resolveInheritedValue('limit', { contest: unsaid, problem: off }),
    null,
  );
  assert.deepEqual(
    resolveInheritedValue('limit', { contest: unsaid, problem: on }),
    { value: 2, source: 'Problem' },
  );
});
