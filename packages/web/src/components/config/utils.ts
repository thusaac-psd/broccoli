import type { FieldProblem, JsonSchema, JsonSchemaProperty } from './types';

export { resolveInheritedValue } from '@broccoli/web-sdk/slot';

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

export function deepMerge(
  base: Record<string, unknown>,
  override: Record<string, unknown>,
): Record<string, unknown> {
  const result = { ...base };
  for (const [key, val] of Object.entries(override)) {
    if (
      val &&
      typeof val === 'object' &&
      !Array.isArray(val) &&
      result[key] &&
      typeof result[key] === 'object' &&
      !Array.isArray(result[key])
    ) {
      result[key] = deepMerge(
        result[key] as Record<string, unknown>,
        val as Record<string, unknown>,
      );
    } else {
      result[key] = val;
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
