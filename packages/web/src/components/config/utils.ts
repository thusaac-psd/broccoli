import type { ApiSchemas } from '@broccoli/web-sdk/api';

import type { MessageKey } from '@/lib/i18n/catalog';

import type {
  FieldProblem,
  InheritedConfig,
  JsonSchema,
  JsonSchemaProperty,
} from './types';

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
  key?: string,
): void {
  for (let i = 0; i < items.length; i++) {
    const itemPath = [...parentPath, String(i)];
    const item = items[i];
    if (itemSchema.type === 'object' && itemSchema.properties) {
      // A new item has no stored secret to keep: one it requires is entered.
      if (isNewItem(item, key)) {
        for (const name of itemSchema.required ?? []) {
          const field = itemSchema.properties[name];
          const value = (item as Record<string, unknown>)[name];
          if (field?.writeOnly && !isEntered(value)) {
            errors[[...itemPath, name].join('.')] = {
              key: 'config.validation.required',
            };
          }
        }
      }
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
      validateItems(value, prop.items, path, errors, prop['x-key']);
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
    case 'type': {
      const key = TYPE_KEYS[field.expected];
      return key ? { key } : serverWords(field);
    }
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
    case 'secret_not_stored':
      return { key: 'config.validation.secretNotStored' };
    case 'secret_required':
      return { key: 'config.validation.secretRequired' };
    case 'namespace_undeclared':
      return { key: 'config.validation.namespaceUndeclared' };
    case 'duplicate_key':
      return {
        key: 'config.validation.duplicateKey',
        params: { value: String(field.value) },
      };
    case 'plugin':
      return {
        pluginKey: field.key ?? null,
        params: messageParams(field.params),
        message: field.message,
      };
    default:
      // Every kind the API names is said above, so a kind added to it fails
      // the build here; one from a newer server reads in its own words.
      field satisfies never;
      return serverWords(field as ServerFieldError);
  }
}

/** A field error said in the server's own words. */
function serverWords(field: ServerFieldError): FieldProblem {
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
    case undefined:
      return '';
  }
}

/** The words a boolean value reads as. */
export interface BooleanWords {
  yes: string;
  no: string;
}

/**
 * A config value as the form shows it wherever it names one (an option, an
 * inherited value, a default): a boolean in words, anything else as text.
 * `undefined` for a value with no such text (none, an object, a list).
 */
export function valueText(
  value: unknown,
  words: BooleanWords,
): string | undefined {
  if (value === undefined || value === null || typeof value === 'object') {
    return undefined;
  }
  if (typeof value === 'boolean') return value ? words.yes : words.no;
  return String(value);
}

/**
 * What a field left at its default reads, as placeholder text: the default
 * the server resolved for a top-level field (`inherited`), else the
 * schema's, as [`valueText`] shows it.
 */
export function defaultText(
  prop: JsonSchemaProperty,
  path: readonly string[],
  say: (value: unknown) => string | undefined,
  inherited?: InheritedConfig,
): string | undefined {
  const [field] = path;
  const resolved =
    path.length === 1 && inherited?.fields?.[field] === 'default'
      ? (inherited.config as Record<string, unknown> | null)?.[field]
      : undefined;
  const shown = resolved ?? prop.default;
  return shown === undefined ? undefined : say(shown);
}

/**
 * The field of a loaded array item that records the key it was saved
 * under. A keyed array's secret belongs to the item, so the form tells the
 * items it read from the server from those added since: an item whose key
 * no longer matches (renamed, or removed and added again) is new, and none
 * of a stored item's secrets is its own. Stripped before a write.
 */
export const SAVED = '$saved';

/**
 * What a write sends in place of a secret to clear it, typed by the API's
 * `ClearSecret` schema, which the server's marker generates: a change on
 * either side fails the build rather than silently clearing nothing.
 */
export const CLEAR_SECRET_MARKER: ApiSchemas['ClearSecret'] = { $clear: true };

/** Whether `value` is the marker that clears a secret. */
export function isClearMarker(value: unknown): boolean {
  return (
    isRecord(value) &&
    Object.keys(value).length === 1 &&
    Object.entries(CLEAR_SECRET_MARKER).every(([k, v]) => value[k] === v)
  );
}

/** The field of the marker a read shows a set secret as. */
const SECRET_REF: keyof ApiSchemas['KeptSecret'] = '$secret';

/**
 * Whether `value` is what a read shows a set secret as, typed by the API's
 * `KeptSecret` schema: the stored secret's ref, which a write sends back at
 * the same item to keep it.
 */
export function isSecretRef(value: unknown): value is ApiSchemas['KeptSecret'] {
  return (
    isRecord(value) &&
    Object.keys(value).length === 1 &&
    typeof value[SECRET_REF] === 'string'
  );
}

/** Whether a secret field holds a value typed into the form. */
function isEntered(value: unknown): boolean {
  return typeof value === 'string' && value !== '';
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** Whether `item` of a keyed array is new: not saved under its key. */
export function isNewItem(item: unknown, key: string | undefined): boolean {
  if (!key || !isRecord(item)) return false;
  return item[SAVED] === undefined || item[SAVED] !== item[key];
}

/**
 * `value` with every item of each keyed array `prop` holds marked as saved
 * under its key ([`SAVED`]): what the form reads, or has just saved.
 */
export function markSaved(value: unknown, prop: JsonSchemaProperty): unknown {
  if (Array.isArray(value)) {
    const key = prop['x-key'];
    return value.map((item) => {
      const marked = prop.items ? markSaved(item, prop.items) : item;
      return key && isRecord(marked) && typeof marked[key] === 'string'
        ? { ...marked, [SAVED]: marked[key] }
        : marked;
    });
  }
  if (!isRecord(value) || !prop.properties) return value;
  return Object.fromEntries(
    Object.entries(value).map(([name, field]) => {
      const child = prop.properties?.[name];
      return [name, child ? markSaved(field, child) : field];
    }),
  );
}

/**
 * `value` as a write sends it: without the saved marks, and with each
 * secret of a new item that holds nothing typed (blank, or the ref of the
 * item it was renamed from) cleared: a ref keeps a secret only at the item
 * a read showed it on, and a stored item of the same key holds none of the
 * new item's.
 */
export function forWrite(
  value: unknown,
  prop: JsonSchemaProperty,
  inNewItem = false,
): unknown {
  if (Array.isArray(value)) {
    const key = prop['x-key'];
    return value.map((item) =>
      prop.items
        ? forWrite(item, prop.items, inNewItem || isNewItem(item, key))
        : item,
    );
  }
  if (!isRecord(value) || !prop.properties) return value;
  const out: Record<string, unknown> = {};
  for (const [name, child] of Object.entries(prop.properties)) {
    const field = value[name];
    if (child.writeOnly && inNewItem && !isEntered(field)) {
      out[name] = { ...CLEAR_SECRET_MARKER };
    } else if (field !== undefined) {
      out[name] = forWrite(field, child, inNewItem);
    }
  }
  for (const [name, field] of Object.entries(value)) {
    if (name !== SAVED && !(name in prop.properties)) out[name] = field;
  }
  return out;
}

/** `value` without the saved marks, as a plugin's slot component reads it. */
export function unmarked(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(unmarked);
  if (!isRecord(value)) return value;
  return Object.fromEntries(
    Object.entries(value)
      .filter(([name]) => name !== SAVED)
      .map(([name, field]) => [name, unmarked(field)]),
  );
}

/** One step of a field's path, as the form names it. */
export type PathStep =
  | { kind: 'field'; path: string[]; title: string }
  | { kind: 'item'; label: string | null; index: number };

/**
 * The steps of the dot path `path` (`stations.0.token`) through `schema`
 * and `values`: each field with its schema title (else its name), and each
 * array item by its key field's value, `null` when it has none.
 */
export function pathSteps(
  path: string,
  schema: JsonSchemaProperty,
  values: unknown,
): PathStep[] {
  const steps: PathStep[] = [];
  const at: string[] = [];
  let prop: JsonSchemaProperty | undefined = schema;
  let value: unknown = values;
  for (const segment of path.split('.').filter(Boolean)) {
    at.push(segment);
    if (/^\d+$/.test(segment) && prop?.type === 'array') {
      const index = Number(segment);
      value = Array.isArray(value) ? value[index] : undefined;
      const key = prop['x-key'];
      const named = key && isRecord(value) ? value[key] : undefined;
      steps.push({
        kind: 'item',
        label: typeof named === 'string' && named !== '' ? named : null,
        index,
      });
      prop = prop.items;
    } else {
      prop = prop?.properties?.[segment];
      value = isRecord(value) ? value[segment] : undefined;
      steps.push({
        kind: 'field',
        path: [...at],
        title: prop?.title ?? segment,
      });
    }
  }
  return steps;
}
