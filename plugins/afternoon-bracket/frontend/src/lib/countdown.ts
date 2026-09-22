// Pure countdown/phase derivation for the Live 小局 view.
//
// Central principle (from the task spec): the countdown is a DISPLAY, not
// an AUTHORITY. The server decides when a 小局 ends, not the client clock.
// If the locally-computed time-until-deadline reaches zero before the
// server has recorded a decision, the correct UI state is "waiting for the
// server", never a fabricated result -- announcing an outcome the server
// has not made would contradict the eventual official one.
//
// NOTE ON THE CURRENT WIRE CONTRACT: `GET /matches/{id}` and `GET /bracket`
// (`MatchView` in plugins/afternoon-bracket/src/routes.rs) do not currently
// return any 小局 timing at all -- no `deadline_ms`, no `opened_at_ms`, no
// `current_xiaoju_index`. `XiaojuTiming` below mirrors the shape of
// `MatchState::xiaoju`'s entries (`plugins/afternoon-bracket/src/model.rs`)
// that WOULD be needed to drive a real countdown; today no caller in this
// package can construct one from a real response, so `LiveXiaojuView`
// always calls `deriveCountdownStatus(null, ...)`. This is flagged as a
// backend/frontend contract gap in this task's report rather than papered
// over with a client-invented deadline, which would be worse than showing
// nothing: a fabricated countdown is exactly the kind of "display treated
// as authority" this module exists to avoid.

export interface XiaojuTiming {
  deadlineMs: number;
  decided: boolean;
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
