import { RequestError } from './errors.ts';

const METHODS = [
  'GET',
  'PUT',
  'POST',
  'DELETE',
  'OPTIONS',
  'HEAD',
  'PATCH',
  'TRACE',
] as const;

/**
 * The client with every failure as a `RequestError`: an error answer comes
 * back as `{ error }` carrying its HTTP status, and a request that gets no
 * answer rejects with a `NETWORK_ERROR` instead of a bare `TypeError`.
 */
export function withRequestErrors<C extends object>(client: C): C {
  const wrapped = { ...client };
  for (const method of METHODS) {
    const send = (client as Record<string, unknown>)[method] as (
      ...args: unknown[]
    ) => Promise<{ error?: unknown; response: Response }>;
    (wrapped as Record<string, unknown>)[method] = async (
      ...args: unknown[]
    ) => {
      let result: { error?: unknown; response: Response };
      try {
        result = await send(...args);
      } catch (cause) {
        if (cause instanceof DOMException && cause.name === 'AbortError') {
          throw cause;
        }
        throw RequestError.network(cause);
      }
      return result.error === undefined
        ? result
        : {
            ...result,
            error: RequestError.fromBody(result.response.status, result.error),
          };
    };
  }
  return wrapped;
}
