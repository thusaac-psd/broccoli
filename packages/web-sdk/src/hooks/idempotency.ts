// Idempotency keys bound to what is being sent.
//
// An Idempotency-Key tells the server "this is the same request again": if it
// already ran, it replays the stored response instead of running it twice.
// That is only safe while the request really is the same. A key kept across an
// edit (a contestant fixes their code after a lost response and submits again)
// makes the server replay the first submission, and the edited code is never
// judged. So keys are derived from the operation itself: retrying the identical
// operation reuses its key, and any change to it (problem, language, files,
// target) gets a fresh one. A key is retired as soon as the server accepts the
// request, so sending the same thing again on purpose is a new operation.

export const IDEMPOTENCY_KEY_MISMATCH = 'IDEMPOTENCY_KEY_MISMATCH';

// The request under this key already succeeded, but the server cannot replay
// its response: it was never stored (it held secrets such as generated
// passwords) or was too large to store. Both are 409s.
export const IDEMPOTENCY_RESPONSE_NOT_STORED =
  'IDEMPOTENCY_RESPONSE_NOT_STORED';
export const IDEMPOTENCY_RESPONSE_TOO_LARGE = 'IDEMPOTENCY_RESPONSE_TOO_LARGE';

// An earlier send under this key is still running on the server (a 409). Once
// it finishes, sending the same key again gets its stored result.
export const IDEMPOTENCY_KEY_IN_PROGRESS = 'IDEMPOTENCY_KEY_IN_PROGRESS';

// An earlier send under this key started but then failed or was cut off (a
// 5xx, a timeout, a cancellation), so it may or may not have taken effect. The
// server answers every later send of that key with this 409 until the key
// expires: it neither replays nor reruns it.
export const IDEMPOTENCY_OUTCOME_UNKNOWN = 'IDEMPOTENCY_OUTCOME_UNKNOWN';

/**
 * Where an idempotency refusal leaves the operation, when it is neither a
 * plain success nor a plain failure:
 *
 * - `already-done`: an earlier send succeeded; only its response is gone.
 * - `outcome-unknown`: an earlier send may or may not have taken effect.
 *   Refetch what it would have changed and have the user check before they
 *   send it again (which then gets a fresh key and runs as a new operation).
 * - `in-progress`: an earlier send is still running. Sending the identical
 *   operation again later reuses its key and gets that send's result.
 */
export type IdempotencyOutcome =
  | 'already-done'
  | 'outcome-unknown'
  | 'in-progress';

const OUTCOMES: ReadonlyMap<unknown, IdempotencyOutcome> = new Map<
  unknown,
  IdempotencyOutcome
>([
  [IDEMPOTENCY_RESPONSE_NOT_STORED, 'already-done'],
  [IDEMPOTENCY_RESPONSE_TOO_LARGE, 'already-done'],
  [IDEMPOTENCY_OUTCOME_UNKNOWN, 'outcome-unknown'],
  [IDEMPOTENCY_KEY_IN_PROGRESS, 'in-progress'],
]);

function codeOf(error: unknown): unknown {
  return typeof error === 'object' && error !== null
    ? (error as { code?: unknown }).code
    : undefined;
}

/**
 * The `IdempotencyOutcome` an error passed on by `sendIdempotent` stands for,
 * or null for any other error (a real failure, to be reported as one).
 */
export function idempotencyOutcome(error: unknown): IdempotencyOutcome | null {
  return OUTCOMES.get(codeOf(error)) ?? null;
}

/**
 * Whether `error` is the server saying an earlier send of this very request
 * already succeeded, though its response is gone. The operation is done: tell
 * the user so (and refetch whatever it changed) instead of reporting a
 * failure, which would invite sending it again as a new operation.
 */
export function isAlreadyDone(error: unknown): boolean {
  return idempotencyOutcome(error) === 'already-done';
}

/**
 * Whether `error` is the server saying an earlier send of this very request
 * may or may not have taken effect. Not a failure to retry blindly: refetch
 * the affected data and have the user check whether it happened first.
 */
export function isOutcomeUnknown(error: unknown): boolean {
  return idempotencyOutcome(error) === 'outcome-unknown';
}

/** How many unaccepted operations keep their key (oldest forgotten first). */
const MAX_PENDING_KEYS = 32;

/**
 * A stable, compact digest of `value`, independent of object key order. Blobs
 * and Files contribute their metadata, not their bytes.
 */
export function fingerprint(value: unknown): string {
  const text = canonical(value);
  return `${hash53(text, 0x9e3779b9)}${hash53(text, 0x85ebca6b)}`;
}

function canonical(value: unknown): string {
  if (value === undefined) return 'u';
  if (typeof value === 'bigint') return `${value}n`;
  if (typeof value === 'function' || typeof value === 'symbol') return 'f';
  if (value === null || typeof value !== 'object') {
    return JSON.stringify(value);
  }
  if (value instanceof Date) return `D${value.getTime()}`;
  if (typeof Blob !== 'undefined' && value instanceof Blob) {
    const file = value as Blob & { name?: string; lastModified?: number };
    return `B${JSON.stringify([file.name, file.size, file.type, file.lastModified])}`;
  }
  if (typeof FormData !== 'undefined' && value instanceof FormData) {
    const entries: unknown[] = [];
    value.forEach((entry, name) => entries.push([name, entry]));
    return `F${canonical(entries)}`;
  }
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  const record = value as Record<string, unknown>;
  // Like JSON, an undefined field is the same as a missing one.
  const keys = Object.keys(record)
    .filter((key) => record[key] !== undefined)
    .sort();
  return `{${keys
    .map((key) => `${JSON.stringify(key)}:${canonical(record[key])}`)
    .join(',')}}`;
}

// cyrb53: a fast 53-bit string hash. Two seeds give ~106 bits, ample to tell
// one operation from another; this is not a security boundary.
function hash53(text: string, seed: number): string {
  let h1 = 0xdeadbeef ^ seed;
  let h2 = 0x41c6ce57 ^ seed;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text.charCodeAt(i);
    h1 = Math.imul(h1 ^ ch, 2654435761);
    h2 = Math.imul(h2 ^ ch, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507);
  h1 ^= Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507);
  h2 ^= Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return (4294967296 * (2097151 & h2) + (h1 >>> 0))
    .toString(36)
    .padStart(11, '0');
}

function randomUuid(): string {
  const bytes = new Uint8Array(16);
  if (typeof globalThis.crypto?.getRandomValues === 'function') {
    globalThis.crypto.getRandomValues(bytes);
  } else {
    for (let i = 0; i < bytes.length; i += 1) {
      bytes[i] = Math.floor(Math.random() * 256);
    }
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0'));
  return [
    hex.slice(0, 4).join(''),
    hex.slice(4, 6).join(''),
    hex.slice(6, 8).join(''),
    hex.slice(8, 10).join(''),
    hex.slice(10, 16).join(''),
  ].join('-');
}

export interface IdempotencyKeys {
  /** The key for an operation, minted on first use. */
  keyFor(operation: string): string;
  /** Forget `key` for `operation`, so its next send gets a fresh one. */
  retire(operation: string, key: string): void;
}

export function createIdempotencyKeys(
  newKey: () => string = randomUuid,
): IdempotencyKeys {
  const keys = new Map<string, string>();
  return {
    keyFor(operation) {
      let key = keys.get(operation);
      if (!key) {
        key = newKey();
        keys.set(operation, key);
        if (keys.size > MAX_PENDING_KEYS) {
          keys.delete(keys.keys().next().value!);
        }
      }
      return key;
    },
    retire(operation, key) {
      if (keys.get(operation) === key) keys.delete(operation);
    },
  };
}

type Verdict = 'accepted' | 'mismatch' | 'failed' | IdempotencyOutcome;

// Whether a verdict retires the key. A failed send keeps it, so retrying the
// identical operation can get a lost result replayed, and so does a send whose
// original is still running, so the retry gets that original's result. Every
// other verdict settles the key: the server will never run it again.
const RETIRES: Readonly<Record<Verdict, boolean>> = {
  accepted: true,
  'already-done': true,
  'outcome-unknown': true,
  mismatch: true,
  'in-progress': false,
  failed: false,
};

function errorVerdict(error: unknown): Verdict {
  if (codeOf(error) === IDEMPOTENCY_KEY_MISMATCH) return 'mismatch';
  return idempotencyOutcome(error) ?? 'failed';
}

// `request` may return an openapi-fetch result ({ data, error, response }) or
// resolve to plain data and throw on failure; both are understood.
function verdictOf(result: unknown): Verdict {
  const response = (result as { response?: unknown } | null)?.response;
  if (typeof Response === 'undefined' || !(response instanceof Response)) {
    return 'accepted';
  }
  if (response.ok) return 'accepted';
  return errorVerdict((result as { error?: unknown }).error);
}

/**
 * Send `request` with the idempotency key for `operation` (everything that
 * defines the request: its target and body).
 *
 * - Accepted: the key is retired; sending the same operation again is new.
 * - Already done (`isAlreadyDone(error)`): an earlier send succeeded but its
 *   response cannot be replayed. The key is retired as if accepted and the
 *   error is passed on, so the caller reports success without the data.
 * - Outcome unknown (`isOutcomeUnknown(error)`): an earlier send was cut off
 *   after it started and may have taken effect. The key is retired (the
 *   server refuses it from now on) and the error is passed on, never resent:
 *   the caller refetches what it would have changed and has the user check
 *   before sending it again, which then gets a fresh key.
 * - In progress (IDEMPOTENCY_KEY_IN_PROGRESS): an earlier send is still
 *   running. The key is kept, so retrying the identical operation later gets
 *   that send's result, and the error is passed on.
 * - Failed (network error, 5xx, validation): the key is kept, so a retry of
 *   the identical operation lets the server replay a result that was lost.
 * - IDEMPOTENCY_KEY_MISMATCH: the server holds a different request under this
 *   key and ran nothing, so the key is retired and the request is sent once
 *   more with a fresh key.
 *
 * `idempotencyOutcome(error)` tells the passed-on refusals apart.
 */
export async function sendIdempotent<T>(
  keys: IdempotencyKeys,
  operation: unknown,
  request: (idempotencyKey: string) => Promise<T>,
): Promise<T> {
  const id = fingerprint(operation);
  for (let attempt = 0; ; attempt += 1) {
    const key = keys.keyFor(id);
    let sent: { ok: true; result: T } | { ok: false; error: unknown };
    let verdict: Verdict;
    try {
      const result = await request(key);
      sent = { ok: true, result };
      verdict = verdictOf(result);
    } catch (error) {
      sent = { ok: false, error };
      verdict = errorVerdict(error);
    }
    if (RETIRES[verdict]) keys.retire(id, key);
    if (verdict === 'mismatch' && attempt === 0) continue;
    if (!sent.ok) throw sent.error;
    return sent.result;
  }
}
