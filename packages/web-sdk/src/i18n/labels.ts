// Names for identifiers the UI shows, in the user's language. Every catalog
// can name them, so the host translates what it owns (roles, queue states)
// and a plugin names what it registers (its contest type, problem type,
// checker), in its own translation pack, without the host knowing it:
//
//   identifier.<kind>.<id>              e.g. identifier.contestType.icpc
//   plugin.<pluginId>.name / .description
//   config.<pluginId>.<namespace>.description
//   config.<pluginId>.<namespace>.<field path>.title / .description
//
// A missing translation shows the text the server sent (the identifier, or
// the manifest's and schema's own text).

/** The kinds of identifiers the UI names. */
export type IdentifierKind =
  | 'contestType'
  | 'problemType'
  | 'checker'
  | 'role'
  | 'queueState';

type Lookup = (key: string) => string | undefined;

export function identifierKey(kind: IdentifierKind, id: string): string {
  return `identifier.${kind}.${id}`;
}

/** The name of `id`, or `id` itself when no catalog names it. */
export function identifierLabel(
  lookup: Lookup,
  kind: IdentifierKind,
  id: string,
): string {
  return lookup(identifierKey(kind, id)) ?? id;
}

/**
 * What the user calls plugin `pluginId`: its translated name
 * (`plugin.<id>.name`), or the id where no catalog names it. Every text that
 * names a plugin (a parked submission, a closed submit, an outage, an error
 * that names one) names it through this, so whoever can act on it sees the
 * same name everywhere.
 */
export function pluginName(lookup: Lookup, pluginId: string): string {
  return lookup(pluginKeys.name(pluginId)) ?? pluginId;
}

/** The catalog keys of a plugin's manifest text. */
export const pluginKeys = {
  name: (pluginId: string) => `plugin.${pluginId}.name`,
  description: (pluginId: string) => `plugin.${pluginId}.description`,
  configDescription: (pluginId: string, namespace: string) =>
    `config.${pluginId}.${namespace}.description`,
  /** A config namespace's short title, which labels its tab. */
  configTitle: (pluginId: string, namespace: string) =>
    `config.${pluginId}.${namespace}.title`,
  /**
   * A schema field's title, description or unit. The key names the field as the
   * schema declares it, so the fields of every item of an array share one
   * key: the item positions in `path` are left out.
   */
  /**
   * The title of option `value` of a schema field's enum. Like a field's
   * own text, the key names the field as the schema declares it.
   */
  configOption: (
    pluginId: string,
    namespace: string,
    path: readonly string[],
    value: string,
  ) =>
    `config.${pluginId}.${namespace}.${path.filter((segment) => !/^\d+$/.test(segment)).join('.')}.options.${value}`,
  configField: (
    pluginId: string,
    namespace: string,
    path: readonly string[],
    part: 'title' | 'description' | 'unit',
  ) =>
    `config.${pluginId}.${namespace}.${path.filter((segment) => !/^\d+$/.test(segment)).join('.')}.${part}`,
};
