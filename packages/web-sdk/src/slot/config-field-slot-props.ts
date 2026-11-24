/**
 * TypeScript types for the props that plugin-registered config field slot
 * components receive. These are passed via `slotProps` when a plugin replaces
 * or wraps a config form field via the `config.field.*` slot.
 */

import type { ApiSchemas } from '@/api/types';

/**
 * What a config form's scope inherits from the scopes above it, as the
 * server resolves it (`GET …/config/cascade`) with the cascade rule plugins
 * read configs by: the config a problem would read with nothing stored at
 * this scope, every field filled, the scope each field comes from
 * (`fields`), and each scope's row as stored (`levels`). A scope the viewer
 * may not read is left out (`null`), and so is each field it sets, named in
 * `withheld`.
 */
export type InheritedConfig = ApiSchemas['ConfigCascadeView'];

export type ConfigScope =
  | { scope: 'plugin'; pluginId: string }
  | { scope: 'contest'; contestId: number }
  | { scope: 'problem'; problemId: number }
  | { scope: 'contest_problem'; contestId: number; problemId: number };

/**
 * Props passed to plugin config form slot components.
 *
 * These are available in `slotProps` when a plugin registers a component
 * for a `config.form.{pluginId}.{namespace}` slot.
 */
export interface ConfigFormSlotProps {
  /** Current config scope being edited. */
  scope?: ConfigScope;
  /** Plugin ID that owns this config namespace. */
  pluginId: string;
  /** Config namespace (e.g. "compilation"). */
  namespace: string;
  /** Current form values (with schema defaults filled in). */
  values: Record<string, unknown>;
  /** Values explicitly stored in the DB (without defaults). */
  storedValues: Record<string, unknown> | null;
  /** JSON schema for this config namespace. */
  schema: Record<string, unknown>;
  /** Whether no explicit values are stored (all values are schema defaults). */
  isUsingDefaultsOnly: boolean;
  /** What this scope inherits from the scopes above it. */
  inherited?: InheritedConfig;
  /** Check whether a field path has an explicit value in the DB. */
  hasExplicitValue: (path: string | string[]) => boolean;
}

/**
 * Props passed to plugin config field slot components.
 *
 * These are available in `slotProps` when a plugin registers a component
 * for a `config.field.{pluginId}.{namespace}.{fieldPath}` slot.
 */
export interface ConfigFieldSlotProps {
  /** Current field value (from the form state). */
  value: unknown;
  /**
   * The field's title in the user's language: the plugin pack's
   * translation of the field (`pluginKeys.configField`), else the schema's
   * `title`, else the field's name. Show this, never `schema.title`, which
   * is the text the plugin wrote in one language.
   */
  label: string;
  /** The field's description, chosen the same way as `label`. */
  description?: string;
  /** JSON schema for this field. */
  schema: {
    type?: string;
    title?: string;
    description?: string;
    default?: unknown;
    enum?: unknown[];
    [key: string]: unknown;
  };
  /** Callback to update this field's value. */
  onChange: (value: unknown) => void;
  /** All form values (root-level). */
  formValues: Record<string, unknown>;
  /** Update any field by path. */
  setFieldValue: (path: string[], value: unknown) => void;
  /** This field's path within the config schema. */
  path: string[];
  /** Current config scope being edited. */
  scope?: ConfigScope;
  /** Whether this field has an explicit value stored in the DB. */
  isExplicitValue: boolean;
  /** Whether any descendant of this field has an explicit value. */
  hasExplicitDescendant: boolean;
  /** Whether the user has modified this field in the current editing session. */
  isDirty: boolean;
  /** What this scope inherits from the scopes above it. */
  inherited?: InheritedConfig;
  /**
   * Whether this field should render as a placeholder (not explicitly set
   * in DB and not yet modified by the user in this session).
   */
  showAsPlaceholder: boolean;
  /**
   * The resolved inherited value for this field from the cascade.
   * `undefined` when no parent scope has an explicit value for this field.
   */
  inheritedValue?: unknown;
  /**
   * The name of the scope the inherited value comes from, in the user's
   * language (e.g., "Contest", "Problem"). `undefined` when no inherited
   * value.
   */
  inheritedSource?: string;
}

/** The value a field inherits, and the scope that sets it. */
export interface InheritedValue {
  /** `undefined` when the value is withheld from the viewer. */
  value: unknown;
  source: Exclude<ApiSchemas['ConfigSource'], 'default' | 'disabled'>;
  /** Whether a scope the viewer may not read sets the value. */
  withheld: boolean;
}

/**
 * The value top-level field `fieldKey` inherits and the scope that sets it,
 * read from what the server resolved; `null` when no scope sets it and the
 * schema's default stands.
 */
export function inheritedValueOf(
  fieldKey: string,
  inherited: InheritedConfig | undefined,
): InheritedValue | null {
  const source = inherited?.fields?.[fieldKey];
  if (!source || source === 'default' || source === 'disabled') return null;
  const config = inherited?.config as Record<string, unknown> | null;
  const withheld = inherited?.withheld.includes(fieldKey) ?? false;
  return { value: withheld ? undefined : config?.[fieldKey], source, withheld };
}
