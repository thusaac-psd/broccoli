import { useEffect, useReducer } from 'react';

/**
 * Whole seconds left of a `seconds` wait the server reported when this
 * machine's monotonic clock (`performance.now()`) read `anchorMs`, now that
 * it reads `nowMs`. The wait is a duration, so it never meets this machine's
 * wall clock, and counting from the anchor stays right however late a tick
 * fires (a hidden tab throttles them).
 */
export function secondsLeft(
  seconds: number,
  anchorMs: number,
  nowMs: number,
): number {
  const elapsed = Math.max(0, nowMs - anchorMs) / 1000;
  return Math.max(0, Math.ceil(seconds - elapsed));
}

/**
 * `secondsLeft` of the wait, re-read while it runs and whenever the page
 * becomes visible again.
 */
export function useSecondsLeft(seconds: number, anchorMs: number): number {
  const [, rerender] = useReducer((n: number) => n + 1, 0);
  const left = secondsLeft(seconds, anchorMs, performance.now());
  const running = left > 0;
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(rerender, 250);
    document.addEventListener('visibilitychange', rerender);
    return () => {
      clearInterval(timer);
      document.removeEventListener('visibilitychange', rerender);
    };
  }, [running, seconds, anchorMs]);
  return left;
}
