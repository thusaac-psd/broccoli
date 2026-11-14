// A client for a plugin's HTTP API, which the host serves under
// `<api base>/p/<plugin id>`. The host knows nothing about the plugin: the
// plugin names itself and its own paths.

import type { IdempotentResult } from '@/hooks/idempotency';

import { RequestError } from './errors.ts';

/** What a plugin request may send: JSON-serializable data, or a raw body. */
export type PluginRequestBody = FormData | Blob | object | undefined;

export interface PluginApi {
  /** GET `path` (with its query string, if any) and read the reply. */
  get<T>(path: string): Promise<T>;
  /**
   * POST `body` to `path` with an idempotency key bound to the request, so a
   * retry after a lost reply is replayed instead of acting twice. Resolves to
   * where the operation stands: settled (with the reply, or no data when an
   * earlier send already succeeded), or not settled, which the caller must
   * handle by refetching and telling the user to check before sending again.
   * Rejects with a `RequestError` when the request failed.
   */
  post<T>(path: string, body?: PluginRequestBody): Promise<IdempotentResult<T>>;
  /** The raw authenticated response, for streams and other non-JSON replies. */
  fetch(path: string, init?: RequestInit): Promise<Response>;
}

type Fetch = (input: string, init?: RequestInit) => Promise<Response>;

/** Sends `request` idempotently and settles it (`settleIdempotent`). */
export type SendSettled = <T>(
  operation: unknown,
  request: (idempotencyKey: string) => Promise<T>,
) => Promise<IdempotentResult<T>>;

/**
 * The reply body: undefined when empty, else parsed as JSON. A body that is
 * not JSON is returned as its text, unless it claims to be JSON (then it is
 * an `INVALID_RESPONSE`). The plugin proxy may send no content type at all.
 */
async function readBody(response: Response): Promise<unknown> {
  const text = await response.text();
  if (!text) return undefined;
  try {
    return JSON.parse(text);
  } catch {
    const type = response.headers.get('content-type') ?? '';
    if (type.includes('json')) {
      throw new RequestError(response.status, 'INVALID_RESPONSE');
    }
    return text;
  }
}

async function toError(response: Response): Promise<RequestError> {
  const body = await readBody(response).catch(() => undefined);
  return RequestError.fromBody(response.status, body);
}

function encode(body: PluginRequestBody): {
  body?: BodyInit;
  headers: Record<string, string>;
} {
  if (body === undefined) return { headers: {} };
  if (
    (typeof FormData !== 'undefined' && body instanceof FormData) ||
    (typeof Blob !== 'undefined' && body instanceof Blob)
  ) {
    // The browser sets the multipart boundary or the blob's type itself.
    return { body: body as BodyInit, headers: {} };
  }
  return {
    body: JSON.stringify(body),
    headers: { 'Content-Type': 'application/json' },
  };
}

export function createPluginApi({
  fetch,
  baseUrl,
  pluginId,
  sendSettled,
}: {
  /** An authenticated fetch for absolute URLs. */
  fetch: Fetch;
  /** The absolute API base URL, e.g. `https://oj.example/api/v1`. */
  baseUrl: string;
  pluginId: string;
  sendSettled: SendSettled;
}): PluginApi {
  const root = `${baseUrl.replace(/\/+$/, '')}/p/${encodeURIComponent(pluginId)}`;

  const send = (path: string, init?: RequestInit) =>
    fetch(`${root}${path}`, init).catch((cause: unknown) => {
      if (cause instanceof DOMException && cause.name === 'AbortError') {
        throw cause;
      }
      throw RequestError.network(cause);
    });

  async function request<T>(path: string, init?: RequestInit): Promise<T> {
    const response = await send(path, init);
    if (!response.ok) throw await toError(response);
    return (await readBody(response)) as T;
  }

  return {
    fetch: send,
    get: (path) => request(path),
    post: (path, body) => {
      const encoded = encode(body);
      return sendSettled({ pluginId, path, body }, (key) =>
        request(path, {
          method: 'POST',
          headers: { 'Idempotency-Key': key, ...encoded.headers },
          body: encoded.body,
        }),
      );
    },
  };
}
