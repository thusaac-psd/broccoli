import type { ApiSchemas } from '@broccoli/web-sdk/api';

import type { MessageKey } from '@/lib/i18n/catalog';

import type { FieldProblem, JsonSchema, JsonSchemaProperty } from './types';

export { inheritedValueOf } from '@broccoli/web-sdk/slot';

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * `override` laid over `base`, as the form shows its scope's own row over
 * what the scope inherits while it is edited: a field `override` sets
 * replaces the one in `base`, except that two objects merge field by field;
 * a field it does not set keeps `base`'s. What a scope inherits is the
 * server's to resolve (`InheritedConfig`); this only lays one row over it.
 */
export function deepMerge(
  base: Record<string, unknown>,
  override: Record<string, unknown>,
): Record<string, unknown> {
  const result = { ...base };
  for (const [key, val] of Object.entries(override)) {
    const below = result[key];
    result[key] =
      isObject(val) && isObject(below) ? deepMerge(below, val) : val;
  }
  return result;
}

export function extractDefaults(schema: JsonSchema): Record<string, unknown> {
  const result: Record<string, unknown> = {};
  if (!schema.properties) return result;
  for (const [key, prop] of Object.entries(schema.properties)) {
    if (prop.default !== undefined) {
      result[key] = prop.default;
    } else if (prop.type === 'object' && prop.properties) {
      result[key] = extractDefaults(prop as JsonSchema);
    }
  }
  return result;
}

/**
 * A copy of `values` with `value` at `path`. Missing containers on the way
 * are created: an array when the next segment is an index, else an object.
 */
export function setAtPath(
  values: Record<string, unknown>,
  path: readonly string[],
  value: unknown,
): Record<string, unknown> {
  if (path.length === 0) return values;
  const next = structuredClone(values);
  let target: Record<string, unknown> = next;
  for (let i = 0; i < path.length - 1; i++) {
    const segment = path[i];
    if (target[segment] === null || typeof target[segment] !== 'object') {
      target[segment] = /^\d+$/.test(path[i + 1]) ? [] : {};
    }
    target = target[segment] as Record<string, unknown>;
  }
  target[path[path.length - 1]] = value;
  return next;
}

export function hasOwnValueAtPath(
  values: Record<string, unknown>,
  path: string[],
): boolean {
  if (path.length === 0) return Object.keys(values).length > 0;

  let current: unknown = values;
  for (const segment of path) {
    if (current === null || typeof current !== 'object') return false;
    if (!Object.hasOwn(current, segment)) return false;
    current = (current as Record<string, unknown>)[segment];
  }
  return true;
}

export function hasOwnDescendantValue(
  values: Record<string, unknown>,
  path: string[],
): boolean {
  if (path.length === 0) return hasAnyOwnValue(values);

  let current: unknown = values;
  for (const segment of path) {
    if (current === null || typeof current !== 'object') return false;
    if (!Object.hasOwn(current, segment)) return false;
    current = (current as Record<string, unknown>)[segment];
  }

  return hasAnyOwnValue(current);
}

function hasAnyOwnValue(value: unknown): boolean {
  if (Array.isArray(value)) return value.length > 0;
  if (value === null || typeof value !== 'object') return value !== undefined;

  const entries = Object.entries(value);
  if (entries.length === 0) return false;
  return entries.some(([, nested]) => hasAnyOwnValue(nested));
}

export function validateField(
  value: unknown,
  prop: JsonSchemaProperty,
): FieldProblem | null {
  if (value === undefined || value === null) return null;

  if (prop.type === 'number' || prop.type === 'integer') {
    if (typeof value !== 'number' || Number.isNaN(value)) {
      return { key: 'config.validation.number' };
    }
    if (prop.type === 'integer' && !Number.isInteger(value)) {
      return { key: 'config.validation.integer' };
    }
    if (prop.minimum !== undefined && value < prop.minimum) {
      return { key: 'config.validation.min', params: { min: prop.minimum } };
    }
    if (prop.maximum !== undefined && value > prop.maximum) {
      return { key: 'config.validation.max', params: { max: prop.maximum } };
    }
  }

  if (prop.type === 'string' && typeof value === 'string') {
    if (prop.minLength !== undefined && value.length < prop.minLength) {
      return {
        key: 'config.validation.minLength',
        params: { min: prop.minLength },
      };
    }
    if (prop.maxLength !== undefined && value.length > prop.maxLength) {
      return {
        key: 'config.validation.maxLength',
        params: { max: prop.maxLength },
      };
    }
  }

  return null;
}

function validateItems(
  items: unknown[],
  itemSchema: JsonSchemaProperty,
  parentPath: string[],
  errors: Record<string, FieldProblem>,
): void {
  for (let i = 0; i < items.length; i++) {
    const itemPath = [...parentPath, String(i)];
    const item = items[i];
    if (itemSchema.type === 'object' && itemSchema.properties) {
      Object.assign(
        errors,
        validateAll(
          (item && typeof item === 'object' ? item : {}) as Record<
            string,
            unknown
          >,
          itemSchema as JsonSchema,
          itemPath,
        ),
      );
    } else if (
      itemSchema.type === 'array' &&
      itemSchema.items &&
      Array.isArray(item)
    ) {
      validateItems(item as unknown[], itemSchema.items, itemPath, errors);
    } else {
      const err = validateField(item, itemSchema);
      if (err) errors[itemPath.join('.')] = err;
    }
  }
}

export function validateAll(
  values: Record<string, unknown>,
  schema: JsonSchema,
  prefix: string[] = [],
): Record<string, FieldProblem> {
  const errors: Record<string, FieldProblem> = {};
  if (!schema.properties) return errors;

  for (const [key, prop] of Object.entries(schema.properties)) {
    const path = [...prefix, key];
    const dotPath = path.join('.');
    const value = values[key];

    if (prop.type === 'object' && prop.properties) {
      const nested = validateAll(
        (value && typeof value === 'object' ? value : {}) as Record<
          string,
          unknown
        >,
        prop as JsonSchema,
        path,
      );
      Object.assign(errors, nested);
    } else if (prop.type === 'array' && prop.items && Array.isArray(value)) {
      validateItems(value, prop.items, path, errors);
    } else {
      const err = validateField(value, prop);
      if (err) errors[dotPath] = err;
    }
  }
  return errors;
}

/** A field error as the server reports it (see `FieldError` in the API). */
type ServerFieldError = ApiSchemas['FieldError'];

const TYPE_KEYS: Record<string, MessageKey> = {
  integer: 'config.validation.integer',
  number: 'config.validation.number',
  string: 'config.validation.string',
  boolean: 'config.validation.boolean',
  array: 'config.validation.array',
  object: 'config.validation.object',
};

/** The string and number values of a plugin's message parameters. */
function messageParams(
  params: Record<string, unknown> | undefined,
): Record<string, string | number> {
  const out: Record<string, string | number> = {};
  for (const [name, value] of Object.entries(params ?? {})) {
    if (typeof value === 'string' || typeof value === 'number') {
      out[name] = value;
    }
  }
  return out;
}

/** What a server field error says, in the UI language when its kind is known. */
export function serverProblem(field: ServerFieldError): FieldProblem {
  switch (field.kind) {
    case 'type':
      if (TYPE_KEYS[field.expected]) return { key: TYPE_KEYS[field.expected] };
      break;
    case 'min':
      return { key: 'config.validation.min', params: { min: field.limit } };
    case 'max':
      return { key: 'config.validation.max', params: { max: field.limit } };
    case 'min_length':
      return {
        key: 'config.validation.minLength',
        params: { min: field.limit },
      };
    case 'max_length':
      return {
        key: 'config.validation.maxLength',
        params: { max: field.limit },
      };
    case 'not_allowed':
      return { key: 'config.validation.notAllowed' };
    case 'pattern':
      return { key: 'config.validation.pattern' };
    case 'unknown_field':
      return { key: 'config.validation.unknownField' };
    case 'missing':
      return { key: 'config.validation.required' };
    case 'plugin':
      return {
        pluginKey: field.key ?? null,
        params: messageParams(field.params),
        message: field.message,
      };
  }
  // A kind this build does not know yet: the server's own words.
  return {
    key: 'plugins.config.serverFieldError',
    params: { message: field.message },
  };
}

/**
 * The field problems of a refused save: the server checks every config
 * against its schema and answers `VALIDATION_ERROR` with `details.fields`,
 * each a `FieldError` whose `path` reads `field.nested[2].leaf`. Keyed like
 * `validateAll`, by dot path with array indices as segments. Empty when the
 * error names no field.
 */
export function serverFieldErrors(
  error: unknown,
): Record<string, FieldProblem> {
  const fields = (error as { details?: { fields?: unknown } } | null)?.details
    ?.fields;
  const errors: Record<string, FieldProblem> = {};
  if (!Array.isArray(fields)) return errors;
  for (const field of fields as ServerFieldError[]) {
    if (typeof field?.path !== 'string' || typeof field.message !== 'string') {
      continue;
    }
    errors[field.path.replace(/\[(\d+)\]/g, '.$1')] = serverProblem(field);
  }
  return errors;
}

export function defaultForType(prop: JsonSchemaProperty): unknown {
  if (prop.default !== undefined) return prop.default;
  switch (prop.type) {
    case 'string':
      return '';
    case 'number':
    case 'integer':
      return 0;
    case 'boolean':
      return false;
    case 'object':
      return prop.properties ? extractDefaults(prop as JsonSchema) : {};
    case 'array':
      return [];
    default:
      return '';
  }
}
