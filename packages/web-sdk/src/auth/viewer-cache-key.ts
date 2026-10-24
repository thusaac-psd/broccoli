/** Keep visibility-filtered caches separate across accounts and permission changes. */
export function viewerCacheKey(
  user: { id: number; permissions: readonly string[] } | null | undefined,
): string {
  return user
    ? `${user.id}:${[...user.permissions].sort().join('|')}`
    : 'guest';
}
