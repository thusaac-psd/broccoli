import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { JsonSchema } from './types.ts';
import {
  deepMerge,
  extractDefaults,
  hasOwnDescendantValue,
  setAtPath,
  validateAll,
  validateField,
} from './utils.ts';

const schema: JsonSchema = {
  type: 'object',
  properties: {
    limit: { type: 'integer', minimum: 1, maximum: 10, default: 3 },
    name: { type: 'string', minLength: 2, maxLength: 4 },
    nested: {
      type: 'object',
      properties: { ratio: { type: 'number', default: 0.5 } },
    },
    weights: { type: 'array', items: { type: 'integer', minimum: 0 } },
  },
};

test('extractDefaults collects defaults, nested objects included', () => {
  assert.deepEqual(extractDefaults(schema), {
    limit: 3,
    nested: { ratio: 0.5 },
  });
});

test('deepMerge merges objects recursively and replaces arrays', () => {
  assert.deepEqual(
    deepMerge(
      { a: 1, nested: { x: 1, y: 2 }, list: [1, 2] },
      { nested: { y: 3 }, list: [9] },
    ),
    { a: 1, nested: { x: 1, y: 3 }, list: [9] },
  );
});

test('setAtPath copies and creates missing containers', () => {
  const base = { nested: { x: 1 } };
  const next = setAtPath(base, ['nested', 'y'], 2);
  assert.deepEqual(next, { nested: { x: 1, y: 2 } });
  assert.deepEqual(base, { nested: { x: 1 } });
  assert.deepEqual(setAtPath({}, ['list', '0'], 'a'), { list: ['a'] });
});

test('validateField reports range and type problems as i18n keys', () => {
  const integer = { type: 'integer' as const, minimum: 1, maximum: 10 };
  assert.equal(validateField(5, integer), null);
  assert.equal(validateField(undefined, integer), null);
  assert.deepEqual(validateField(1.5, integer), {
    key: 'config.validation.integer',
  });
  assert.deepEqual(validateField(Number.NaN, integer), {
    key: 'config.validation.number',
  });
  assert.deepEqual(validateField(0, integer), {
    key: 'config.validation.min',
    params: { min: 1 },
  });
  assert.deepEqual(validateField(11, integer), {
    key: 'config.validation.max',
    params: { max: 10 },
  });
  const text = { type: 'string' as const, minLength: 2, maxLength: 4 };
  assert.deepEqual(validateField('a', text), {
    key: 'config.validation.minLength',
    params: { min: 2 },
  });
  assert.deepEqual(validateField('abcde', text), {
    key: 'config.validation.maxLength',
    params: { max: 4 },
  });
});

test('validateAll keys problems by dotted path, into arrays', () => {
  assert.deepEqual(
    Object.keys(
      validateAll({ limit: 0, name: 'ok', weights: [1, -1] }, schema),
    ),
    ['limit', 'weights.1'],
  );
  assert.deepEqual(validateAll({ limit: 2, nested: { ratio: 1 } }, schema), {});
});

test('hasOwnDescendantValue ignores empty containers', () => {
  assert.equal(hasOwnDescendantValue({ nested: {} }, []), false);
  assert.equal(hasOwnDescendantValue({ nested: { x: 0 } }, ['nested']), true);
  assert.equal(hasOwnDescendantValue({ list: [] }, ['list']), false);
});
