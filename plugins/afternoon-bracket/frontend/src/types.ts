// Wire types for the afternoon-bracket plugin's 6 HTTP routes. Kept in
// lockstep with the Rust source of truth:
//   - `MatchPhase`              -- plugins/afternoon-bracket/src/model.rs
//   - `MatchView` (GET routes)  -- plugins/afternoon-bracket/src/routes.rs
//   - order/start/force-decide  -- plugins/afternoon-bracket/src/{ordering,routes}.rs
//
// IMPORTANT: `MatchView` (what `GET /bracket` and `GET /matches/{id}`
// return) does NOT include any 小局 timing (no `deadline_ms`,
// `opened_at_ms`, or `current_xiaoju_index`) -- only `state`, `score_a`,
// `score_b`, `winner`, `decided_at_ms`, and (while `state ==
// "awaiting_judge"`) `awaiting_submission_id`. `XiaojuTiming` in
// `./lib/countdown.ts` is therefore never populated from a real response
// today; see that module's doc comment and this task's report for the
// backend gap this leaves.

export type MatchPhase =
  | 'pending'
  | 'ordering'
  | 'in_progress'
  | 'tiebreak'
  | 'awaiting_judge'
  | 'decided'
  | 'needs_adjudication';

/**
 * A group/order triple as returned by `GET /bracket` / `GET /matches/{id}`:
 * fixed-position, per-entry visibility masking. A masked entry is `null` but
 * KEEPS its array position -- it does not collapse or reorder. See
 * `mask_group` in `plugins/afternoon-bracket/src/routes.rs`.
 */
export type MaskedTriple = [number | null, number | null, number | null];

/** One match, visibility-filtered for the requesting viewer. */
export interface MatchView {
  id: number;
  round: number;
  pos: number;
  player_a: number;
  player_b: number;
  group_a: MaskedTriple;
  group_b: MaskedTriple;
  order_a: MaskedTriple | null;
  order_b: MaskedTriple | null;
  tiebreak_problem: number | null;
  score_a: number;
  score_b: number;
  state: MatchPhase;
  winner: number | null;
  decided_at_ms: number;
  /**
   * The id of the in-flight submission this match is blocked on. Present
   * only while `state === 'awaiting_judge'`; never masked (structural, like
   * `state` itself).
   */
  awaiting_submission_id: number | null;
}

export interface BracketResponse {
  matches: MatchView[];
}

/**
 * `POST /matches/{id}/order` response. NOT the same shape as `MatchView`'s
 * `order_a`/`order_b`: this handler returns the raw (unmasked)
 * `MatchState.order_a`/`order_b` fields directly, not a `match_view`-masked
 * projection -- see `handle_order` in
 * plugins/afternoon-bracket/src/ordering.rs. `null` here means "not
 * submitted yet", not "masked".
 */
export interface OrderResponse {
  order_a: [number, number, number] | null;
  order_b: [number, number, number] | null;
}

/** `POST /matches/{id}/order` request body. */
export interface OrderRequest {
  order: [number, number, number];
}

export interface StartResponse {
  state: MatchPhase;
}

export interface ForceDecideRequest {
  /** Absent/`null` = force expiry (re-run `advance` at the current time). */
  winner?: number | null;
}

export interface ForceDecideResponse {
  state: MatchPhase;
  winner: number | null;
}
