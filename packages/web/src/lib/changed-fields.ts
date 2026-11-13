/**
 * The fields of `after` that differ from `before`: what a PATCH should send,
 * so fields the user did not touch are left as the server has them (another
 * editor may have changed them meanwhile).
 */
export function changedFields<T extends Record<string, unknown>>(
  before: T,
  after: T,
): Partial<T> {
  const changed: Partial<T> = {};
  for (const key of Object.keys(after) as (keyof T)[]) {
    if (JSON.stringify(before[key]) !== JSON.stringify(after[key])) {
      changed[key] = after[key];
    }
  }
  return changed;
}
