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

/** The catalog keys of a plugin's manifest text. */
export const pluginKeys = {
  name: (pluginId: string) => `plugin.${pluginId}.name`,
  description: (pluginId: string) => `plugin.${pluginId}.description`,
  configDescription: (pluginId: string, namespace: string) =>
    `config.${pluginId}.${namespace}.description`,
  configField: (
    pluginId: string,
    namespace: string,
    path: readonly string[],
    part: 'title' | 'description',
  ) => `config.${pluginId}.${namespace}.${path.join('.')}.${part}`,
};
