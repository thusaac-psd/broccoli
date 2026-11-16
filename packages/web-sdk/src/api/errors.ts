import type { components } from '@/api/schema';

/**
 * The `{ code, message, details? }` error envelope returned by every API
 * endpoint on failure. Mirrors the server's `ErrorBody` schema component.
 */
export type ApiError = components['schemas']['ErrorBody'];

/**
 * The code of a request that never got an answer: the server or the network
 * is down, or the browser is offline.
 */
export const NETWORK_ERROR = 'NETWORK_ERROR';

/** The code of an error answer without an `ErrorBody` (a proxy error page). */
export const HTTP_ERROR = 'HTTP_ERROR';

/**
 * A failed API request, from the typed client, a plugin API or
 * `toRequestError`. It is an `ApiError` envelope that also knows how the
 * request failed: `status` is the HTTP status, or null when no answer came
 * (`code` is then `NETWORK_ERROR`). `message` is the server's own text, for
 * logs and as secondary detail; what the user reads comes from
 * `describeError`.
 */
export class RequestError extends Error implements ApiError {
  readonly status: number | null;
  readonly code: string;
  readonly details?: unknown;

  constructor(
    status: number | null,
    code: string,
    message = '',
    details?: unknown,
  ) {
    super(message);
    this.name = 'RequestError';
    this.status = status;
    this.code = code;
    this.details = details;
  }

  /** The error for an answer with `status` and the parsed `body`. */
  static fromBody(status: number, body: unknown): RequestError {
    const fields =
      typeof body === 'object' && body !== null
        ? (body as Record<string, unknown>)
        : {};
    const message = [fields.message, fields.error].find(
      (value): value is string => typeof value === 'string' && value !== '',
    );
    return new RequestError(
      status,
      typeof fields.code === 'string' ? fields.code : HTTP_ERROR,
      message ?? '',
      fields.details,
    );
  }

  /** The error for a request that got no answer. */
  static network(cause?: unknown): RequestError {
    const error = new RequestError(null, NETWORK_ERROR);
    if (cause !== undefined) (error as { cause?: unknown }).cause = cause;
    return error;
  }
}

/**
 * An error whose message is already translated text for the user (a check
 * the client made itself, or a failure already described). `describeError`
 * shows it as it is.
 */
export class TranslatedError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'TranslatedError';
  }
}

/** The `RequestError` for a failed raw `Response` (from `useApiFetch`). */
export async function toRequestError(
  response: Response,
): Promise<RequestError> {
  const body = await response
    .clone()
    .json()
    .catch(() => undefined);
  return RequestError.fromBody(response.status, body);
}

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
 * What a failure means for the person who sees it, which decides what they
 * are offered: sign in, retry, or nothing.
 *
 * - `network`: no answer; retry once the connection is back.
 * - `temporary`: the server is busy; retry shortly.
 * - `unauthenticated`: a 401; sign in (again).
 * - `forbidden`: a 403; signed in, but not allowed.
 * - `notFound`: a 404.
 * - `rateLimited`: a 429.
 * - `conflict`: a 409; the state changed or already exists.
 * - `invalid`: any other 4xx; the request itself was refused.
 * - `server`: a 5xx, or a failure that is not a request error at all.
 */
export type ErrorKind =
  | 'network'
  | 'temporary'
  | 'unauthenticated'
  | 'forbidden'
  | 'notFound'
  | 'rateLimited'
  | 'conflict'
  | 'invalid'
  | 'server';

function statusOf(err: unknown): number | null | undefined {
  if (err instanceof RequestError) return err.status;
  if (typeof err === 'object' && err !== null && 'status' in err) {
    const status = (err as { status: unknown }).status;
    if (typeof status === 'number') return status;
  }
  return undefined;
}

export function errorKind(err: unknown): ErrorKind {
  const parsed = parseApiError(err);
  if (parsed && TEMPORARY_ERROR_CODES.includes(parsed.code)) return 'temporary';
  const status = statusOf(err);
  // `fetch` rejects with a TypeError when no answer comes.
  if (status === null || err instanceof TypeError) return 'network';
  if (status === undefined) return 'server';
  if (status === 401) return 'unauthenticated';
  if (status === 403) return 'forbidden';
  if (status === 404) return 'notFound';
  if (status === 409) return 'conflict';
  if (status === 429) return 'rateLimited';
  if (status === 503) return 'temporary';
  if (status >= 400 && status < 500) return 'invalid';
  return 'server';
}

/**
 * Whether `err` is a temporary error: show it as "busy, retrying" and retry,
 * never as a failure of the request.
 */
export function isTemporaryApiError(err: unknown): boolean {
  return errorKind(err) === 'temporary';
}

/** The translation lookups `describeError` needs (from `useTranslation`). */
export interface ErrorTranslator {
  lookup: (
    key: string,
    params?: Record<string, string | number>,
  ) => string | undefined;
}

/** The catalog key that translates the error code `code`. */
export function errorCodeKey(code: string): string {
  return `apiError.${code}`;
}

/** The catalog key of the generic message for an `ErrorKind`. */
export function errorKindKey(kind: ErrorKind): string {
  return `apiError.kind.${kind}`;
}

/** `details` as message parameters: its top-level strings and numbers. */
function detailParams(details: unknown): Record<string, string | number> {
  const params: Record<string, string | number> = {};
  if (typeof details !== 'object' || details === null) return params;
  for (const [key, value] of Object.entries(details)) {
    if (typeof value === 'string' || typeof value === 'number') {
      params[key] = value;
    }
  }
  return params;
}

/** What to tell the user about a failure, in their language. */
export interface ErrorDescription {
  kind: ErrorKind;
  /** The translated text to show. */
  message: string;
}

/**
 * Describe `err` for the user, in their language, never in the server's
 * English:
 *
 * 1. The translation of its code (`apiError.<CODE>`), from the host catalog
 *    or the pack of the plugin that sent it, with the error's `details` as
 *    parameters.
 * 2. For a refusal without one (`invalid`, `conflict`), `failure`: what the
 *    caller was doing went wrong ("Could not save the problem"), followed by
 *    the server's own text as detail, which helps whoever set things up.
 * 3. Otherwise the generic message for its kind ("The server cannot be
 *    reached"), which says more than `failure` would.
 */
export function describeError(
  err: unknown,
  failure: string,
  { lookup }: ErrorTranslator,
): ErrorDescription {
  const kind = errorKind(err);
  if (err instanceof TranslatedError) return { kind, message: err.message };
  const parsed = parseApiError(err);
  const byCode =
    parsed && lookup(errorCodeKey(parsed.code), detailParams(parsed.details));
  if (byCode) return { kind, message: byCode };
  if (kind === 'invalid' || kind === 'conflict') {
    const detail = parsed?.message;
    return {
      kind,
      message: detail
        ? (lookup('apiError.withDetail', { message: failure, detail }) ??
          `${failure} (${detail})`)
        : failure,
    };
  }
  return { kind, message: lookup(errorKindKey(kind)) ?? failure };
}
