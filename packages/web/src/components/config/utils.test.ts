import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { JsonSchema } from './types.ts';
import {
  defaultText,
  extractDefaults,
  forWrite,
  hasOwnDescendantValue,
  isSecretRef,
  markSaved,
  pathSteps,
  serverFieldErrors,
  setAtPath,
  storedRef,
  validateAll,
  validateField,
  valueText,
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

test('serverFieldErrors says each server field error in the UI language, keyed like validateAll', () => {
  const error = {
    code: 'VALIDATION_ERROR',
    details: {
      fields: [
        {
          path: 'penalty_minutes',
          message: 'expected integer, got string',
          kind: 'type',
          expected: 'integer',
          got: 'string',
        },
        {
          path: 'subtasks[1].max_score',
          message: 'must be at most 10',
          kind: 'max',
          limit: 10,
        },
        {
          path: 'name',
          message: 'must be at least 2 characters',
          kind: 'min_length',
          limit: 2,
        },
        {
          path: 'mode',
          message: 'must be one of "a"',
          kind: 'not_allowed',
          allowed: ['a'],
        },
        { path: 'bogus', message: 'unknown field', kind: 'unknown_field' },
        { path: '', message: 'something new', kind: 'from_a_newer_server' },
      ],
    },
  };
  assert.deepEqual(serverFieldErrors(error), {
    penalty_minutes: { key: 'config.validation.integer' },
    'subtasks.1.max_score': {
      key: 'config.validation.max',
      params: { max: 10 },
    },
    name: { key: 'config.validation.minLength', params: { min: 2 } },
    mode: { key: 'config.validation.notAllowed' },
    bogus: { key: 'config.validation.unknownField' },
    '': {
      key: 'plugins.config.serverFieldError',
      params: { message: 'something new' },
    },
  });
  assert.deepEqual(serverFieldErrors({ code: 'NOT_FOUND' }), {});
  assert.deepEqual(serverFieldErrors(null), {});
});
test("a plugin's refusal keeps its pack key, its parameters and its words", () => {
  const errors = serverFieldErrors({
    details: {
      fields: [
        {
          path: 'subtasks[0].name',
          message: "another subtask is already named 'a'",
          kind: 'plugin',
          key: 'ioi.config.task.nameTaken',
          params: { name: 'a', nested: { dropped: true } },
        },
        { path: 'stations[1].token', message: 'taken', kind: 'plugin' },
      ],
    },
  });
  assert.deepEqual(errors['subtasks.0.name'], {
    pluginKey: 'ioi.config.task.nameTaken',
    params: { name: 'a' },
    message: "another subtask is already named 'a'",
  });
  assert.deepEqual(errors['stations.1.token'], {
    pluginKey: null,
    params: {},
    message: 'taken',
  });
});

const words = { yes: 'Yes', no: 'No' };
const say = (value: unknown) => valueText(value, words);

test('a boolean reads in words, other values as text', () => {
  assert.equal(
    defaultText({ type: 'boolean', default: false }, ['on'], say),
    'No',
  );
  assert.equal(valueText(true, { yes: '是', no: '否' }), '是');
  assert.equal(valueText('fast', words), 'fast');
  assert.equal(valueText(['x'], words), undefined);
});

test('a field left at its default reads its default, the resolved one first', () => {
  const limit = schema.properties!.limit;
  assert.equal(defaultText(limit, ['limit'], say), '3');
  assert.equal(
    defaultText({ type: 'integer', default: 0 }, ['retries'], say),
    '0',
    'a default of zero is a default',
  );
  assert.equal(defaultText({ type: 'integer' }, ['free'], say), undefined);
  assert.equal(
    defaultText(limit, ['limit'], say, {
      config: { limit: 7 },
      source: 'default',
      is_enabled: true,
      fields: { limit: 'default' },
      withheld: [],
      levels: {
        contest_problem: null,
        contest: null,
        problem: null,
        plugin: null,
      },
    }),
    '7',
    'the default the server resolved',
  );
  assert.equal(
    defaultText(
      schema.properties!.nested.properties!.ratio,
      ['nested', 'ratio'],
      say,
    ),
    '0.5',
  );
  assert.equal(
    defaultText(schema.properties!.weights, ['weights'], say),
    undefined,
  );
});

const stations: JsonSchema = {
  type: 'object',
  properties: {
    stations: {
      type: 'array',
      title: 'Print stations',
      'x-key': 'name',
      items: {
        type: 'object',
        required: ['name', 'token'],
        properties: {
          name: { type: 'string', title: 'Station name' },
          token: { type: 'string', title: 'Token', writeOnly: true },
        },
      },
    },
  },
};

test('a saved item sends its ref back, and a new or renamed one clears what it holds untyped', () => {
  const loaded = markSaved(
    {
      stations: [
        { name: 'alpha', token: { $secret: 'ra' } },
        { name: 'beta', token: { $secret: 'rb' } },
      ],
    },
    stations,
  ) as { stations: Record<string, unknown>[] };
  const [alpha, beta] = loaded.stations;
  const edited = {
    stations: [
      beta,
      { ...alpha, name: 'renamed' },
      { name: 'alpha', token: '' },
      { name: 'typed', token: 'new' },
    ],
  };
  assert.deepEqual(forWrite(edited, stations), {
    stations: [
      { name: 'beta', token: { $secret: 'rb' } },
      { name: 'renamed', token: { $clear: true } },
      { name: 'alpha', token: { $clear: true } },
      { name: 'typed', token: 'new' },
    ],
  });
});

test('a set secret reads as the marker of its ref, and nothing else does', () => {
  assert.equal(isSecretRef({ $secret: 'r' }), true);
  assert.equal(isSecretRef({ $secret: 'r', x: 1 }), false);
  assert.equal(isSecretRef({ $clear: true }), false);
  assert.equal(isSecretRef(null), false);
  assert.equal(isSecretRef('r'), false);
});

test('a new item that requires its secret has one entered before it is saved', () => {
  const values = markSaved(
    { stations: [{ name: 'alpha', token: { $secret: 'ra' } }] },
    stations,
  ) as { stations: Record<string, unknown>[] };
  const [alpha] = values.stations;
  const errors = validateAll(
    {
      stations: [
        alpha,
        { name: 'new', token: '' },
        { ...alpha, name: 'renamed' },
      ],
    },
    stations,
  );
  assert.deepEqual(Object.keys(errors), [
    'stations.1.token',
    'stations.2.token',
  ]);
});

test('a field path reads as its titles and its items by their key', () => {
  const values = { stations: [{ name: 'desk' }, { name: '' }] };
  assert.deepEqual(pathSteps('stations.0.token', stations, values), [
    { kind: 'field', path: ['stations'], title: 'Print stations' },
    { kind: 'item', label: 'desk', index: 0 },
    { kind: 'field', path: ['stations', '0', 'token'], title: 'Token' },
  ]);
  assert.deepEqual(pathSteps('stations.1', stations, values)[1], {
    kind: 'item',
    label: null,
    index: 1,
  });
});

test("a field's stored secret is its own item's, by the key it was saved under", () => {
  const stored = markSaved(
    {
      stations: [
        { name: 'alpha', token: { $secret: 'ra' } },
        { name: 'beta', token: { $secret: 'rb' } },
      ],
    },
    stations,
  ) as { stations: Record<string, unknown>[] };
  const [, beta] = stored.stations;
  const moved = { stations: [{ ...beta, token: 'typed' }] };
  assert.deepEqual(storedRef(stored, moved, ['stations', '0', 'token']), {
    $secret: 'rb',
  });
  assert.equal(
    storedRef(stored, { stations: [{ name: 'new', token: '' }] }, [
      'stations',
      '0',
      'token',
    ]),
    null,
    'an item added since the read has no stored secret',
  );
});
