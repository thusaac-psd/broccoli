/** Statement and sample cache entries are always scoped to a viewer and their permissions. */
export function isProblemContentQuery(
  key: readonly unknown[],
  viewerId: number,
  ids?: ReadonlySet<number>,
): boolean {
  return (
    (key[0] === 'problem' || key[0] === 'problem-sample-contents') &&
    typeof key[1] === 'number' &&
    typeof key[3] === 'string' &&
    key[3].startsWith(`${viewerId}:`) &&
    (!ids || ids.has(key[1]))
  );
}
