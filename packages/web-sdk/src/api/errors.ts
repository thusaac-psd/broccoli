import type { components } from '@/api/schema';

/**
 * The `{ code, message, details? }` error envelope returned by every API
 * endpoint on failure. Mirrors the server's `ErrorBody` schema component.
 */
export type ApiError = components['schemas']['ErrorBody'];

/**
 * Narrow an unknown thrown value or response body to the server's
 * `ApiError` envelope. Returns `null` when the value doesn't match.
 */
export function parseApiError(value: unknown): ApiError | null {
  if (typeof value !== 'object' || value === null) return null;
  const obj = value as Record<string, unknown>;
  if (typeof obj.code === 'string' && typeof obj.message === 'string') {
    return value as ApiError;
  }
  return null;
}

/**
 * Error codes that mean "this server is busy right now" (HTTP 503 with a
 * `Retry-After`): the request was neither refused nor answered, and the same
 * request succeeds once load drops.
 */
export const TEMPORARY_ERROR_CODES: readonly string[] = [
  'PLUGIN_UNAVAILABLE',
  'QUEUE_OVERLOADED',
  'CONTEST_TYPE_UNAVAILABLE',
];

/**
 * Whether `err` is a temporary error: show it as "busy, retrying" and retry,
 * never as a failure of the request.
 */
export function isTemporaryApiError(err: unknown): boolean {
  const parsed = parseApiError(err);
  return parsed !== null && TEMPORARY_ERROR_CODES.includes(parsed.code);
}

/**
 * Extract a human-readable error message from an API error envelope or a
 * generic Error. Falls back to the provided fallback string.
 */
export function getErrorMessage(err: unknown, fallback: string): string {
  if (!err) return fallback;

  // API error envelopes and Error instances both carry { message }.
  if (typeof err === 'object' && 'message' in err) {
    const msg = (err as { message?: unknown }).message;
    if (msg && typeof msg === 'string') return msg;
  }

  if (err instanceof Error) return err.message;

  return fallback;
}
