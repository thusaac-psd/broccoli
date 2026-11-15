import type { ApiSchemas } from '@broccoli/web-sdk/api';

export type IncompatibleConfig = ApiSchemas['IncompatibleConfig'];

/**
 * The stored configs a refused test-data change would break: what a
 * `CONFIG_WOULD_BREAK` answer lists in `details.issues`. `null` for any other
 * outcome.
 */
export function configBreakIssues(error: unknown): IncompatibleConfig[] | null {
  const body = error as {
    code?: unknown;
    details?: { issues?: unknown };
  } | null;
  if (body?.code !== 'CONFIG_WOULD_BREAK') return null;
  const issues = body.details?.issues;
  return Array.isArray(issues) ? (issues as IncompatibleConfig[]) : [];
}
