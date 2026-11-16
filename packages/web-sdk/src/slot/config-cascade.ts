/**
 * The config cascade's rule for the admin forms, the one the server SDK's
 * `resolve_cascade` judges with: each scope that stores a namespace sets the
 * fields it stores and leaves the rest to the next scope up (contest problem,
 * contest, problem, the plugin-wide row, then the schema's defaults). It
 * mirrors `resolve_cascade` across the language boundary; see the config
 * cascade entry of `docs/plans/dedup-backlog.md`.
 */
import type {
  InheritedConfig,
  InheritedValue,
  ParentScopeConfig,
} from './config-field-slot-props.ts';

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * `override` laid over `base`: a field `override` sets replaces the one in
 * `base`, except that two objects merge field by field; a field it does not
 * set keeps `base`'s.
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

/**
 * The value a field inherits from the parent scopes, and the most specific
 * scope that sets it; `null` when none does. The most specific parent scope
 * that says whether the config is on decides it, and a switched-off scope
 * contributes no values; when the deciding scope is switched off nothing is
 * inherited.
 */
export function resolveInheritedValue(
  fieldKey: string,
  inherited: InheritedConfig | undefined,
): InheritedValue | null {
  if (!inherited) return null;
  // Most specific first, only the scopes that store the namespace.
  const scopes = (
    [
      [inherited.contest, 'Contest'],
      [inherited.problem, 'Problem'],
    ] as [ParentScopeConfig | undefined, string][]
  ).filter(
    (entry): entry is [ParentScopeConfig & { values: object }, string] =>
      entry[0]?.values != null,
  );
  const decided = scopes.find(([scope]) => scope.enabled !== null)?.[0];
  if (decided?.enabled === false) return null;

  let found: InheritedValue | null = null;
  for (const [scope, source] of scopes.reverse()) {
    if (scope.enabled === false) continue;
    const value = scope.values[fieldKey];
    if (value === undefined) continue;
    found = {
      value:
        found && isObject(found.value) && isObject(value)
          ? deepMerge(found.value, value)
          : value,
      source,
    };
  }
  return found;
}
