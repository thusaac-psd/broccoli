// Pure countdown/phase derivation for the Live 小局 view.
//
// Central principle (from the task spec): the countdown is a DISPLAY, not
// an AUTHORITY. The server decides when a 小局 ends, not the client clock.
// If the locally-computed time-until-deadline reaches zero before the
// server has recorded a decision, the correct UI state is "waiting for the
// server", never a fabricated result -- announcing an outcome the server
// has not made would contradict the eventual official one.
//
// WIRE CONTRACT (commit 2378a986): `MatchView.current_xiaoju_deadline_ms`
// (plus `_index`/`_opened_at_ms`) carries the currently-open 小局's real
// server deadline, all three `None`-together before the first 小局 opens.
// See `xiaojuTimingFromMatch` below for how a real `MatchView` is turned
// into the `XiaojuTiming` this module's pure derivation expects -- in
// particular why "decided" comes from `MatchView.state`, not from these
// fields becoming absent (they are not guaranteed to, once the match ends;
// see that function's doc comment).

import type { MatchPhase } from '../types.ts';

export interface XiaojuTiming {
  deadlineMs: number;
  decided: boolean;
}

/**
 * Build the `XiaojuTiming` `deriveCountdownStatus` expects from a real
 * `MatchView`. Returns `null` ("no clock") when
 * `current_xiaoju_deadline_ms` is absent -- before the first 小局 opens.
 *
 * `decided` is derived from `state`, deliberately NOT from whether these
 * timing fields are present: the server does not clear
 * `MatchState::xiaoju` once a match concludes (`apply_force_decide` and the
 * natural win path both only flip `state`), so a decided match's deadline
 * fields can keep reporting the last-played 小局's now-stale deadline
 * rather than going back to `None`. Gating "decided" on `state` instead
 * means `deriveCountdownStatus` reports `{ kind: 'decided' }` correctly
 * either way, rather than treating a stale-but-present deadline as if the
 * match were still counting down.
 *
 * This also makes `MatchPhase.AwaitingJudge` fall out of the existing pure
 * derivation for free: its deadline is frozen in the past (the block began
 * because the deadline already passed) and `decided` is `false` (the
 * server has not resolved it yet), so `deriveCountdownStatus` naturally
 * returns `waiting-for-server` -- exactly what `AwaitingJudge` means --
 * without this function needing a special case for it.
 */
export function xiaojuTimingFromMatch(match: {
  current_xiaoju_deadline_ms: number | null;
  state: MatchPhase;
}): XiaojuTiming | null {
  if (match.current_xiaoju_deadline_ms === null) {
    return null;
  }
  return {
    deadlineMs: match.current_xiaoju_deadline_ms,
    decided: match.state === 'decided' || match.state === 'needs_adjudication',
  };
}

export type CountdownStatus =
  | { kind: 'no-data' }
  | { kind: 'counting'; remainingMs: number }
  | { kind: 'waiting-for-server'; overdueMs: number }
  | { kind: 'decided' };

export function deriveCountdownStatus(
  timing: XiaojuTiming | null,
  nowMs: number,
): CountdownStatus {
  if (timing === null) {
    return { kind: 'no-data' };
  }
  if (timing.decided) {
    // The server has already recorded a decision. This takes priority over
    // the deadline math below even if `nowMs` is still before
    // `deadlineMs` -- a 小局 can end early (e.g. both players' current-best
    // submissions are already known), and once `decided` is true, showing a
    // ticking countdown next to a result that already exists would be
    // actively misleading.
    return { kind: 'decided' };
  }
  const remainingMs = timing.deadlineMs - nowMs;
  if (remainingMs > 0) {
    return { kind: 'counting', remainingMs };
  }
  // The local clock says time is up, but `decided` is still false: an older
  // submission may still be in flight (this is exactly the situation
  // `MatchPhase::AwaitingJudge` names on the server side). Show a waiting
  // state, not a result -- see the module doc comment.
  // `Math.abs`, not unary negation: when `remainingMs` is exactly `0`,
  // `-remainingMs` is `-0`, which is not `Object.is`-equal to `0` --
  // `Math.abs(-0) === 0` (positive zero) avoids that trap.
  return { kind: 'waiting-for-server', overdueMs: Math.abs(remainingMs) };
}

/** Format a non-negative millisecond duration as `M:SS` (e.g. `1:05`). */
export function formatDurationMs(ms: number): string {
  const totalSeconds = Math.max(0, Math.round(ms / 1000));
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${seconds.toString().padStart(2, '0')}`;
}

/**
 * Human-readable label for a `CountdownStatus`, shared by every component
 * that renders one so the exhaustiveness check lives in exactly one place
 * (see this task's requirement that no phase/status ever fall through to a
 * default branch). The `switch` below has no `default` arm on purpose: if a
 * future `CountdownStatus` variant is added without a case here, this stops
 * compiling instead of silently rendering nothing for it.
 */
export function formatCountdownLabel(
  status: CountdownStatus,
  t: (key: string, params?: Record<string, string | number>) => string,
): string {
  switch (status.kind) {
    case 'no-data':
      return t('afternoon-bracket.countdown.noData');
    case 'counting':
      return t('afternoon-bracket.countdown.counting', {
        time: formatDurationMs(status.remainingMs),
      });
    case 'waiting-for-server':
      return t('afternoon-bracket.countdown.waitingForServer', {
        time: formatDurationMs(status.overdueMs),
      });
    case 'decided':
      return t('afternoon-bracket.countdown.decided');
  }
}
