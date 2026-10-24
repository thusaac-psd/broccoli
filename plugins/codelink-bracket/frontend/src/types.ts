// Wire types for the codelink-bracket plugin's HTTP routes. Kept in
// lockstep with the Rust source of truth:
//   - `MatchPhase`              -- plugins/codelink-bracket/src/model.rs
//   - `MatchView` (GET routes)  -- plugins/codelink-bracket/src/routes.rs
//   - order/start/force-decide  -- plugins/codelink-bracket/src/{ordering,routes}.rs
//
// `current_xiaoju_index`/`current_xiaoju_deadline_ms`/
// `current_xiaoju_opened_at_ms` (added by commit 2378a986) are read from a
// single `xiaoju.last()` on the server, so they are all-`None` or all-`Some`
// together -- never a mix. `None` before the first 小局 opens; see
// `./lib/countdown.ts`'s `xiaojuTimingFromMatch` for how the UI turns these
// into a countdown without trusting them as more authoritative than
// `state` (a stale-but-`Some` deadline can outlive the 小局 it describes --
// see that function's doc comment).

export type MatchPhase =
  | 'pending'
  | 'ordering'
  | 'preparing'
  | 'intermission'
  | 'in_progress'
  | 'tiebreak'
  | 'awaiting_judge'
  | 'decided'
  | 'needs_adjudication';

export type MatchKind = 'elimination' | 'final' | 'third_place';

/**
 * A problem group/order as returned by `GET /bracket` / `GET /matches/{id}`:
 * fixed-position, per-entry visibility masking. A masked entry is `null` but
 * KEEPS its array position -- it does not collapse or reorder. See
 * `mask_group` in `plugins/codelink-bracket/src/routes.rs`.
 */
export type MaskedGroup = (number | null)[];

/** One match, visibility-filtered for the requesting viewer. */
export interface MatchView {
  /** Only present for this viewer's active match. */
  own_feedback?: {
    penalty_count: number;
    delay_seconds: number;
    pending_submission_id: number | null;
    effective_at_us: number | null;
  };
  id: number;
  round: number;
  stage_count: number;
  kind: MatchKind;
  third_place_enabled: boolean;
  stage_start_at_ms: number | null;
  ordering_deadline_ms: number | null;
  next_game_at_ms: number | null;
  pos: number;
  player_a: number;
  player_b: number;
  /** Usernames for `player_a`/`player_b`; null only if the server's lookup failed. */
  player_a_name: string | null;
  player_b_name: string | null;
  group_a: MaskedGroup;
  group_b: MaskedGroup;
  order_a: MaskedGroup | null;
  order_b: MaskedGroup | null;
  tiebreak_problem: number | null;
  /** First-to target for regular-game wins. Group length is the BO cap. */
  wins_to_advance: number;
  score_a: number;
  score_b: number;
  state: MatchPhase;
  winner: number | null;
  decided_at_ms: number;
  /**
   * The id of the in-flight submission this match is blocked on. Set while
   * `state === 'awaiting_judge'` and kept if that block escalates to
   * `needs_adjudication`. Never masked (structural, like `state` itself).
   */
  awaiting_submission_id: number | null;
  /**
   * Why the match needs staff; set only in `needs_adjudication`. Null for
   * matches escalated before the server recorded reasons.
   */
  adjudication_reason: AdjudicationReason | null;
  /**
   * The currently open (or most recently open) 小局's index/deadline/open
   * time, from `MatchState::xiaoju.last()`. `None` before the first 小局
   * opens. Never masked -- structural, like `state`.
   *
   * NOT guaranteed `None` once the match is `decided`/`needs_adjudication`:
   * the server does not clear `xiaoju` on decision, so these can keep
   * reporting the last-played 小局's (now-stale) deadline. Do not use
   * field-presence alone to decide whether a match is still live -- use
   * `state` for that (see `xiaojuTimingFromMatch`).
   */
  current_xiaoju_index: number | null;
  current_xiaoju_deadline_ms: number | null;
  current_xiaoju_opened_at_ms: number | null;
  /** Optional for responses from a server predating shared extra time. */
  current_game_base_deadline_ms?: number | null;
  match_extra_time_seconds?: number;
  /** Balance after settled games; use the current deadlines for the live clock. */
  extra_time_remaining_ms?: number;
  /**
   * Every game opened so far, in order. Problem ids are masked exactly like
   * `order_a`/`order_b`; games not yet opened are absent.
   */
  games: GameView[];
  /**
   * While ranking: when the match starts by itself (contest start or the
   * opening of the stage); null while a ranking is still missing. Null
   * once the match has started.
   */
  starts_at_ms: number | null;
}

/** One opened game (小局). The tiebreak flag distinguishes regular games from tiebreaks. */
export interface GameView {
  index: number;
  tiebreak: boolean;
  problem_a: number | null;
  problem_b: number | null;
  opened_at_ms: number;
  deadline_ms: number;
  base_deadline_ms?: number;
  extra_time_used_ms?: number;
  winner: number | null;
  decided: boolean;
}

export interface BracketResponse {
  player_count: number;
  third_place_enabled?: boolean;
  total_matches?: number;
  stages?: StageView[];
  matches: MatchView[];
  /** The contest's end time; null (or absent) if the server could not read it. */
  contest_end_ms?: number | null;
}

export interface StageView {
  round: number;
  phase:
    | 'scheduled'
    | 'awaiting_reschedule'
    | 'ordering'
    | 'preparing'
    | 'playing'
    | 'complete';
  start_at_ms: number | null;
  ordering_deadline_ms: number | null;
  starts_at_ms: number | null;
  match_count: number;
  decided_count: number;
}

export type AdjudicationReason =
  | 'stuck_judge'
  | 'tiebreak_exhausted'
  | 'setup_missing'
  | 'contest_ended';

/**
 * `POST /matches/{id}/order` response. NOT the same shape as `MatchView`'s
 * `order_a`/`order_b`: this handler returns the raw (unmasked)
 * `MatchState.order_a`/`order_b` fields directly, not a `match_view`-masked
 * projection -- see `handle_order` in
 * plugins/codelink-bracket/src/ordering.rs. `null` here means "not
 * submitted yet", not "masked".
 */
export interface OrderResponse {
  order_a: number[] | null;
  order_b: number[] | null;
}

/** `POST /matches/{id}/order` request body. */
export interface OrderRequest {
  order: number[];
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

/** One round's problems and timings for `POST /setup` (see `RoundDef` in src/model.rs). */
export interface RoundSetup {
  group_a: number[];
  group_b: number[];
  tiebreak: number[];
  /** Omission or null uses a majority of the regular game count. */
  wins_to_advance?: number | null;
  start_at_ms?: number | null;
  post_order_wait_seconds?: number | null;
  game_break_seconds?: number | null;
  match_extra_time_seconds?: number | null;
  xiaoju_seconds: number;
  tiebreak_seconds: number;
  break_before_seconds: number;
}

/** `POST /setup` body. Seeds pair up adjacently in round 1. */
export interface SetupRequest {
  seeds: number[];
  rounds: RoundSetup[];
  third_place?:
    | { mode: 'shared' }
    | ({ mode: 'custom' } & Pick<
        RoundSetup,
        'group_a' | 'group_b' | 'tiebreak'
      >)
    | null;
  match_extra_time_seconds: number;
  penalty_seconds: number;
  escalation_grace_seconds: number;
}

/** Staff-only full setup, including the immutable prefix of opened stages. */
export interface SetupResponse {
  setup: (SetupRequest & { opened_rounds: number }) | null;
  started: boolean;
  contest_start_ms?: number | null;
  contest_end_ms?: number | null;
}
