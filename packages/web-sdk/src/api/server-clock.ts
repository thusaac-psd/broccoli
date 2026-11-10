import { useEffect, useReducer, useSyncExternalStore } from 'react';

// The server decides when a contest starts and ends, by its own clock. A
// contestant's PC often has no NTP, and a dual-boot PC whose RTC holds local
// time can run hours off, so anything that asks "has the contest started?"
// must ask by the server's clock. This module estimates the offset between
// the two from the `Date` header of responses the app receives anyway (no
// extra requests), and every contest-timing computation goes through
// `serverNow()` instead of `Date.now()`.
//
// `Date` has whole-second resolution and is stamped somewhere between when the
// request was sent and when the response arrived, so each response bounds the
// offset to an interval. Intersecting the intervals of successive responses
// converges on the true offset well below a second. An empty intersection
// means one of the clocks jumped (the user fixed their clock, NTP kicked in),
// so the estimate starts over from the newest response.
//
// Cross-origin, `Date` is readable only when the server exposes it; until then
// no sample arrives and the local clock is used unchanged.

/** Offset changes smaller than this do not re-render subscribers. */
const PUBLISH_THRESHOLD_MS = 250;

export function createServerClock(localNow: () => number = Date.now) {
  let lowerBound = -Infinity;
  let upperBound = Infinity;
  let offsetMs = 0;
  let publishedOffsetMs = 0;
  const listeners = new Set<() => void>();

  return {
    /**
     * Feed one response's `Date` header into the estimate. `sentAt` and
     * `receivedAt` are local clock readings around the request.
     */
    observe(dateHeader: string | null, sentAt: number, receivedAt: number) {
      if (!dateHeader) return;
      const serverMs = Date.parse(dateHeader);
      if (!Number.isFinite(serverMs) || receivedAt < sentAt) return;

      const sampleLower = serverMs - receivedAt;
      const sampleUpper = serverMs + 1_000 - sentAt;
      const lower = Math.max(lowerBound, sampleLower);
      const upper = Math.min(upperBound, sampleUpper);
      [lowerBound, upperBound] =
        lower <= upper ? [lower, upper] : [sampleLower, sampleUpper];
      offsetMs = Math.round((lowerBound + upperBound) / 2);

      if (Math.abs(offsetMs - publishedOffsetMs) >= PUBLISH_THRESHOLD_MS) {
        publishedOffsetMs = offsetMs;
        listeners.forEach((listener) => listener());
      }
    },
    /** The current time by the server's clock, in epoch milliseconds. */
    now: () => localNow() + offsetMs,
    /** The offset as last announced to subscribers. */
    publishedOffset: () => publishedOffsetMs,
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}

const clock = createServerClock();

/** `fetch` that also feeds the server clock. Every API request goes through it. */
export async function fetchWithServerClock(
  input: RequestInfo | URL,
  init?: RequestInit,
): Promise<Response> {
  const sentAt = Date.now();
  const response = await fetch(input, init);
  clock.observe(response.headers.get('Date'), sentAt, Date.now());
  return response;
}

/**
 * The current time by the server's clock, in epoch milliseconds. Use it, not
 * `Date.now()`, for anything the server enforces by time (contest windows).
 */
export function serverNow(): number {
  return clock.now();
}

function useServerClockOffset(): number {
  return useSyncExternalStore(clock.subscribe, clock.publishedOffset, () => 0);
}

/**
 * The current server time, re-read whenever the clock estimate moves and,
 * with `tickMs`, on that interval (e.g. 1000 for a countdown).
 */
export function useServerNow(tickMs?: number): number {
  useServerClockOffset();
  const [, tick] = useReducer((n: number) => n + 1, 0);
  useEffect(() => {
    if (!tickMs) return;
    const timer = setInterval(tick, tickMs);
    return () => clearInterval(timer);
  }, [tickMs]);
  return serverNow();
}

/**
 * Whether the server clock has reached `atMs`, re-rendering once when it
 * does. `false` while `atMs` is unknown.
 */
export function useServerTimeReached(atMs: number | null): boolean {
  const offset = useServerClockOffset();
  const [checks, recheck] = useReducer((n: number) => n + 1, 0);
  const reached = atMs !== null && serverNow() >= atMs;
  useEffect(() => {
    if (atMs === null || reached) return;
    // setTimeout fires at once for delays past 2^31-1 ms (~24.8 days).
    const delay = Math.min(atMs - serverNow() + 50, 2 ** 31 - 1);
    const timer = setTimeout(recheck, delay);
    return () => clearTimeout(timer);
  }, [atMs, reached, offset, checks]);
  return reached;
}
