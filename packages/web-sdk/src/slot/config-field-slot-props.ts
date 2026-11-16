/**
 * TypeScript types for the props that plugin-registered config field slot
 * components receive. These are passed via `slotProps` when a plugin replaces
 * or wraps a config form field via the `config.field.*` slot.
 */

/** Inherited config from a parent scope in the cascade. */
export interface ParentScopeConfig {
  values: Record<string, unknown> | null;
  /** Whether the scope switches the config on or off; `null` leaves that to
   * the next scope up. */
  enabled: boolean | null;
}

/** Inherited config from parent scopes (contest and/or problem). */
export interface InheritedConfig {
  contest?: ParentScopeConfig;
  problem?: ParentScopeConfig;
}

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
  /** Inherited config from parent scopes. */
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
  /** Full inherited config from parent scopes (raw). */
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
   * The human-readable source label for the inherited value
   * (e.g., "Contest", "Problem"). `undefined` when no inherited value.
   */
  inheritedSource?: string;
}

/** Resolved inherited value for a field. */
export interface InheritedValue {
  value: unknown;
  source: string;
}
