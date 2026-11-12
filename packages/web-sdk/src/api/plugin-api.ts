// A client for a plugin's HTTP API, which the host serves under
// `<api base>/p/<plugin id>`. The host knows nothing about the plugin: the
// plugin names itself and its own paths.

/** A failed plugin API request: its HTTP status and the error body's code. */
export class PluginApiError extends Error {
  readonly status: number;
  readonly code?: string;
  readonly details?: unknown;

  constructor(
    message: string,
    status: number,
    code?: string,
    details?: unknown,
  ) {
    super(message);
    this.name = 'PluginApiError';
    this.status = status;
    this.code = code;
    this.details = details;
  }
}

export interface PluginApi {
  /** GET `path` (with its query string, if any) and parse the JSON reply. */
  get<T>(path: string): Promise<T>;
  /**
   * POST `body` (sent as JSON when given) to `path` with an idempotency key
   * bound to the request: retrying the identical request after a lost reply
   * reuses the key, so the server replays the result instead of acting twice.
   */
  post<T>(path: string, body?: unknown): Promise<T>;
  /** The raw authenticated response, for streams and other non-JSON replies. */
  fetch(path: string, init?: RequestInit): Promise<Response>;
}

type Fetch = (input: string, init?: RequestInit) => Promise<Response>;

/** Sends `request` with the idempotency key for `operation`. */
export type SendIdempotent = <T>(
  operation: unknown,
  request: (idempotencyKey: string) => Promise<T>,
) => Promise<T>;

async function readJson(response: Response): Promise<unknown> {
  const text = await response.text();
  return text ? JSON.parse(text) : undefined;
}

async function toError(response: Response): Promise<PluginApiError> {
  const body = (await readJson(response).catch(() => undefined)) as
    | { code?: unknown; message?: unknown; error?: unknown; details?: unknown }
    | undefined;
  const message =
    [body?.message, body?.error].find(
      (value): value is string => typeof value === 'string' && value !== '',
    ) ?? `Request failed: ${response.status}`;
  return new PluginApiError(
    message,
    response.status,
    typeof body?.code === 'string' ? body.code : undefined,
    body?.details,
  );
}

export function createPluginApi({
  fetch,
  baseUrl,
  pluginId,
  sendIdempotent,
}: {
  /** An authenticated fetch for absolute URLs. */
  fetch: Fetch;
  /** The absolute API base URL, e.g. `https://oj.example/api/v1`. */
  baseUrl: string;
  pluginId: string;
  sendIdempotent: SendIdempotent;
}): PluginApi {
  const root = `${baseUrl.replace(/\/+$/, '')}/p/${encodeURIComponent(pluginId)}`;

  const send = (path: string, init?: RequestInit) =>
    fetch(`${root}${path}`, init);

  async function request<T>(path: string, init?: RequestInit): Promise<T> {
    const response = await send(path, init);
    if (!response.ok) throw await toError(response);
    return (await readJson(response)) as T;
  }

  return {
    fetch: send,
    get: (path) => request(path),
    post: (path, body) =>
      sendIdempotent({ pluginId, path, body }, (key) =>
        request(path, {
          method: 'POST',
          headers: {
            'Idempotency-Key': key,
            ...(body === undefined
              ? {}
              : { 'Content-Type': 'application/json' }),
          },
          body: body === undefined ? undefined : JSON.stringify(body),
        }),
      ),
  };
}
