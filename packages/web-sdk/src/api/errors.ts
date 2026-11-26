import type { components } from '@/api/schema';

import { formatList } from '../i18n/format.ts';
import { pluginName } from '../i18n/labels.ts';

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
 * `describeError`. `retryAfterMs` is the answer's `Retry-After`, the
 * soonest the server asks to be asked again, or null without one.
 */
export class RequestError extends Error implements ApiError {
  readonly status: number | null;
  readonly code: string;
  readonly details?: unknown;
  readonly retryAfterMs: number | null;

  constructor(
    status: number | null,
    code: string,
    message = '',
    details?: unknown,
    retryAfterMs: number | null = null,
  ) {
    super(message);
    this.name = 'RequestError';
    this.status = status;
    this.code = code;
    this.details = details;
    this.retryAfterMs = retryAfterMs;
  }

  /** The error for the error answer `response`, whose parsed body is `body`. */
  static fromResponse(response: Response, body: unknown): RequestError {
    return RequestError.fromBody(
      response.status,
      body,
      parseRetryAfter(response.headers.get('retry-after')),
    );
  }

  /**
   * The error for an answer with `status` and the parsed `body`, and the
   * `retryAfterMs` its `Retry-After` asks for.
   */
  static fromBody(
    status: number,
    body: unknown,
    retryAfterMs: number | null = null,
  ): RequestError {
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
      retryAfterMs,
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
 * A `Retry-After` header in milliseconds. The server sends whole seconds; an
 * HTTP date names an instant on the server's clock, which the browser's
 * cannot be compared with, so it counts as no header.
 */
function parseRetryAfter(header: string | null): number | null {
  if (header === null || !/^\s*\d+\s*$/.test(header)) return null;
  return Number(header) * 1_000;
}

/** The `Retry-After` of a failure in milliseconds, or null without one. */
export function retryAfterOf(err: unknown): number | null {
  return err instanceof RequestError ? err.retryAfterMs : null;
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
  return RequestError.fromResponse(response, body);
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
 * `null` for a failure that refuses this viewer the resource (403, 404): the
 * view has nothing to show them, such as a participant's panel on the page
 * of a contest they do not take part in. Any other failure is rethrown, as
 * `promise.catch(nothingForThisViewer)` keeps it.
 */
export function nothingForThisViewer(err: unknown): null {
  const kind = errorKind(err);
  if (kind === 'forbidden' || kind === 'notFound') return null;
  throw err;
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
  /** The UI language, for the lists a message names. */
  locale: string;
  lookup: (
    key: string,
    params?: Record<string, string | number>,
  ) => string | undefined;
}

/**
 * The catalog key that translates the error code `code`. The host sends a
 * plugin's code as `<plugin id>.<CODE>` (a plugin id holds no `.`), whether a
 * plugin route or hook refused the request or a judgement or code run
 * records it: its translation is `<plugin id>.apiError.<CODE>` in that
 * plugin's pack, and only there, so a plugin's code never reads as the
 * host's condition of the same name. A code without a plugin id is the
 * host's own, translated by `apiError.<CODE>`.
 */
export function errorCodeKey(code: string): string {
  const dot = code.indexOf('.');
  if (dot <= 0) return `apiError.${code}`;
  return `${code.slice(0, dot)}.apiError.${code.slice(dot + 1)}`;
}

/**
 * The translation of the error code `code` (`errorCodeKey`), or `undefined`
 * when no pack has one, for the caller's generic message.
 */
export function errorCodeText(
  code: string,
  { lookup }: Pick<ErrorTranslator, 'lookup'>,
): string | undefined {
  return lookup(errorCodeKey(code)) || undefined;
}

/** The catalog key of the generic message for an `ErrorKind`. */
export function errorKindKey(kind: ErrorKind): string {
  return `apiError.kind.${kind}`;
}

/**
 * `details` as message parameters: its top-level strings and numbers, and its
 * lists of them (such as the `usernames` a conflict names) joined as the
 * UI language lists names (`formatList`).
 * A `plugin_id` is also given as `plugin`, the plugin's name (`pluginName`).
 */
function detailParams(
  details: unknown,
  { lookup, locale }: ErrorTranslator,
): Record<string, string | number> {
  const params: Record<string, string | number> = {};
  if (typeof details !== 'object' || details === null) return params;
  for (const [key, value] of Object.entries(details)) {
    if (typeof value === 'string' || typeof value === 'number') {
      params[key] = value;
    } else if (Array.isArray(value) && value.every(isScalar)) {
      params[key] = formatList(value.map(String), locale);
    }
  }
  if (typeof params.plugin_id === 'string') {
    params.plugin = pluginName(lookup, params.plugin_id);
  }
  return params;
}

function isScalar(value: unknown): value is string | number {
  return typeof value === 'string' || typeof value === 'number';
}

/** What to tell the user about a failure, in their language. */
export interface ErrorDescription {
  kind: ErrorKind;
  /** The translated text to show. */
  message: string;
}

/** Whether `kind` is a refusal: the server answered, and said no. */
function isRefusal(kind: ErrorKind): boolean {
  switch (kind) {
    case 'unauthenticated':
    case 'forbidden':
    case 'notFound':
    case 'rateLimited':
    case 'conflict':
    case 'invalid':
      return true;
    case 'network':
    case 'temporary':
    case 'server':
      return false;
  }
}

/**
 * Whether asking again, as it is, can succeed: no answer, a busy or broken
 * server, a rate limit that lapses. Only these offer the user a retry.
 */
export function retryCanFix(kind: ErrorKind): boolean {
  switch (kind) {
    case 'network':
    case 'temporary':
    case 'rateLimited':
    case 'server':
      return true;
    case 'unauthenticated':
    case 'forbidden':
    case 'notFound':
    case 'conflict':
    case 'invalid':
      return false;
  }
}

/**
 * Describe `err` for the user, in their language:
 *
 * 1. The translation of its code (`errorCodeKey`): a host code from the
 *    host catalog, a plugin's only from that plugin's pack, with the error's
 *    `details` as parameters.
 * 2. For a refusal without one (any 4xx) where the server said why,
 *    `failure`, what the caller was doing ("Could not print"), followed by
 *    the server's own words ("Print limit reached"): they are specific, and
 *    a generic text for the kind could only guess.
 * 3. For a refusal the server said nothing about, the generic message for
 *    its kind ("You do not have permission to do this"), or `failure` for a
 *    bad request or a conflict, which their kind says nothing more about.
 * 4. For no answer or a server failure, the generic message for its kind
 *    ("The server cannot be reached"), never the server's internal text.
 */
export function describeError(
  err: unknown,
  failure: string,
  translator: ErrorTranslator,
): ErrorDescription {
  const { lookup } = translator;
  const kind = errorKind(err);
  if (err instanceof TranslatedError) return { kind, message: err.message };
  const parsed = parseApiError(err);
  const byCode =
    parsed &&
    lookup(errorCodeKey(parsed.code), detailParams(parsed.details, translator));
  if (byCode) return { kind, message: byCode };
  const detail = parsed?.message;
  if (isRefusal(kind) && detail) {
    return {
      kind,
      message:
        lookup('apiError.withDetail', { message: failure, detail }) ??
        `${failure} (${detail})`,
    };
  }
  if (kind === 'invalid' || kind === 'conflict')
    return { kind, message: failure };
  return { kind, message: lookup(errorKindKey(kind)) ?? failure };
}
