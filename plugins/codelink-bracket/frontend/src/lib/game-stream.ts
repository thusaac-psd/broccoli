import { errorKind, RequestError } from '@broccoli/web-sdk/api';

import type { BracketResponse } from '../types.ts';
import { readEventStream } from './event-stream.ts';
import { type GameNotification, gameUpdates } from './notifications.ts';

/**
 * How soon a stream that ended or failed is opened again, and how long a
 * stream must have lived to count as healthy rather than as a failure.
 */
export const RECONNECT_MS = 1_000;

/** What a viewer's game stream reads from and does, passed in. */
export interface GameStream {
  /** Opens the bracket's event stream. */
  open: (signal: AbortSignal) => Promise<Response>;
  /** A monotonic clock, in milliseconds. */
  now: () => number;
  /**
   * Waits `ms` before the next attempt, or less once `signal` aborts or,
   * when `wakesOnline`, the browser comes back online.
   */
  wait: (
    ms: number,
    wakesOnline: boolean,
    signal: AbortSignal,
  ) => Promise<void>;
  /** The wait after `failures` failed attempts in a row, the last `error`. */
  retryDelay: (failures: number, error: unknown) => number;
  /** Whether the stream is connected now. */
  connected: (on: boolean) => void;
  /** Every snapshot the stream sends, for the caches. */
  snapshot: (snapshot: BracketResponse) => void;
  /** A game of the viewer's decided since the games it has seen. */
  notify: (
    notification: GameNotification,
    signal: AbortSignal,
  ) => void | Promise<void>;
  /** The games the viewer has seen, kept across reconnects and reloads. */
  seen: { read: () => string[] | null; write: (seen: string[]) => void };
  viewerId: number;
  signal: AbortSignal;
}

/**
 * Follows the viewer's bracket stream until `signal` aborts or the server
 * says the viewer may not read it. Each snapshot is compared with the games
 * the viewer has seen, so a game decided while the stream was down is
 * notified once, from the first snapshot after it reconnects. A stream that
 * lived `RECONNECT_MS` (the server closes healthy ones to reauthorize) is
 * opened again at once; one that failed or ended sooner waits the backoff.
 */
export async function runGameStream(stream: GameStream): Promise<void> {
  const { signal } = stream;
  let seen = stream.seen.read();
  let failures = 0;
  let lastError: unknown;
  while (!signal.aborted) {
    const started = stream.now();
    try {
      const response = await stream.open(signal);
      if ([401, 403, 404].includes(response.status)) return;
      if (
        !response.ok ||
        !response.body ||
        !response.headers.get('content-type')?.startsWith('text/event-stream')
      ) {
        throw RequestError.fromResponse(response, null);
      }
      stream.connected(true);
      await readEventStream(response.body, async (event) => {
        if (signal.aborted || event.event !== 'snapshot') return;
        const snapshot = JSON.parse(event.data) as BracketResponse;
        stream.snapshot(snapshot);
        const update = gameUpdates(snapshot.matches, stream.viewerId, seen);
        for (const notification of update.notifications) {
          if (signal.aborted) return;
          await stream.notify(notification, signal);
        }
        seen = update.seen;
        stream.seen.write(seen);
      });
      if (stream.now() - started >= RECONNECT_MS) {
        failures = 0;
        continue;
      }
      lastError = undefined;
    } catch (error) {
      if (signal.aborted) return;
      lastError = error;
    } finally {
      if (!signal.aborted) stream.connected(false);
    }
    failures += 1;
    // Coming back online ends the wait only after a failure that was the
    // network's: a wait the server asked for (`Retry-After`) stands.
    await stream.wait(
      stream.retryDelay(failures, lastError),
      errorKind(lastError) === 'network',
      signal,
    );
  }
}
