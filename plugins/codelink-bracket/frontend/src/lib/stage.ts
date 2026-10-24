// Where a player stands in their own match, for the "My match" panel. Pure so
// the mapping from server state to what the player is told can be tested
// directly, like `phase.ts` does for the generic status label.

import type { MatchView } from '../types.ts';

export type Side = 'a' | 'b';

export type PlayerStage =
  | { kind: 'waiting_opponent' }
  | { kind: 'rank' }
  /** Ranked; `startsAtMs` null means the opponent has not ranked yet. */
  | { kind: 'waiting_start'; startsAtMs: number | null }
  | { kind: 'preparing'; untilMs: number | null }
  | { kind: 'intermission'; untilMs: number | null }
  | { kind: 'third_place_pending' }
  | { kind: 'play'; game: number; problemId: number | null }
  | { kind: 'judging' }
  | { kind: 'staff' }
  | { kind: 'advanced' }
  | { kind: 'eliminated' };

export function sideOf(match: MatchView, viewerId: number | null): Side | null {
  if (viewerId === null) return null;
  if (match.player_a === viewerId) return 'a';
  if (match.player_b === viewerId) return 'b';
  return null;
}

/**
 * A player ranks the OPPONENT's problems, and that ranking becomes the
 * opponent's order: player A's submission lands in `order_b` (see
 * `MatchState` in src/model.rs). So "have I ranked yet" reads the other
 * side's order, and "what do I solve" reads my own.
 */
export function playerStage(match: MatchView, side: Side): PlayerStage {
  const ownOrder = side === 'a' ? match.order_a : match.order_b;
  const submittedRanking = side === 'a' ? match.order_b : match.order_a;
  switch (match.state) {
    case 'pending':
      return { kind: 'waiting_opponent' };
    case 'ordering':
      if (match.ordering_deadline_ms != null) return { kind: 'rank' };
      return submittedRanking === null
        ? { kind: 'rank' }
        : { kind: 'waiting_start', startsAtMs: match.starts_at_ms };
    case 'preparing':
      return { kind: 'preparing', untilMs: match.starts_at_ms };
    case 'intermission':
      return { kind: 'intermission', untilMs: match.next_game_at_ms };
    case 'in_progress': {
      const game = match.current_xiaoju_index ?? 0;
      return { kind: 'play', game, problemId: ownOrder?.[game] ?? null };
    }
    case 'tiebreak':
      return {
        kind: 'play',
        game: match.current_xiaoju_index ?? match.group_a.length,
        problemId: match.tiebreak_problem,
      };
    case 'awaiting_judge':
      return { kind: 'judging' };
    case 'needs_adjudication':
      return { kind: 'staff' };
    case 'decided': {
      const me = side === 'a' ? match.player_a : match.player_b;
      if (
        match.winner !== me &&
        match.third_place_enabled &&
        match.round === match.stage_count - 1
      ) {
        return { kind: 'third_place_pending' };
      }
      return match.winner === me
        ? { kind: 'advanced' }
        : { kind: 'eliminated' };
    }
  }
}

export function finishingPlace(match: MatchView, side: Side): number | null {
  if (match.state !== 'decided' || match.round !== match.stage_count)
    return null;
  const won = match.winner === (side === 'a' ? match.player_a : match.player_b);
  return match.kind === 'third_place' ? (won ? 3 : 4) : won ? 1 : 2;
}

export type StepStatus = 'done' | 'current' | 'upcoming';

export interface Step {
  key: 'rank' | 'start' | 'game' | 'tiebreak' | 'result';
  game?: number;
  status: StepStatus;
}

/**
 * The stepper shown above the player's current task: rank, start, the configured regular
 * games, a tiebreak step only once one has opened, then the result.
 */
export function matchSteps(match: MatchView, side: Side): Step[] {
  const stage = playerStage(match, side);
  const opened = match.games.length;
  const hasTiebreak =
    match.state === 'tiebreak' || match.games.some((g) => g.tiebreak);
  const beforePlay =
    stage.kind === 'waiting_opponent' ||
    stage.kind === 'rank' ||
    stage.kind === 'waiting_start' ||
    stage.kind === 'preparing';

  const steps: Step[] = [
    {
      key: 'rank',
      status:
        stage.kind === 'rank' || stage.kind === 'waiting_opponent'
          ? 'current'
          : 'done',
    },
    {
      key: 'start',
      status:
        stage.kind === 'waiting_start' || stage.kind === 'preparing'
          ? 'current'
          : beforePlay
            ? 'upcoming'
            : 'done',
    },
  ];
  const current = match.current_xiaoju_index;
  const live =
    match.state === 'in_progress' ||
    match.state === 'tiebreak' ||
    match.state === 'awaiting_judge';
  // An FT win can finish before the BO cap. Unplayed games are absent from
  // the finished match, rather than appearing as upcoming forever.
  const regularGames =
    match.state === 'decided'
      ? match.games.filter((g) => !g.tiebreak).length
      : match.group_a.length;
  for (let game = 0; game < regularGames; game++) {
    steps.push({
      key: 'game',
      game,
      status:
        live && current === game
          ? 'current'
          : game < opened
            ? 'done'
            : 'upcoming',
    });
  }
  if (hasTiebreak) {
    steps.push({
      key: 'tiebreak',
      status:
        live && (current ?? 0) >= match.group_a.length
          ? 'current'
          : opened > match.group_a.length
            ? 'done'
            : 'upcoming',
    });
  }
  steps.push({
    key: 'result',
    status:
      match.state === 'decided'
        ? 'done'
        : match.state === 'needs_adjudication'
          ? 'current'
          : 'upcoming',
  });
  return steps;
}

/** Use the configured group length for regular-game and tiebreak numbering. */
export function gameLabel(
  match: MatchView,
  index: number,
  short = false,
): [string, { n: number }] {
  const tiebreak = index >= match.group_a.length;
  const key = tiebreak
    ? short
      ? 'shortTiebreak'
      : 'tiebreak'
    : short
      ? 'short'
      : 'regular';
  return [
    `codelink-bracket.game.${key}`,
    { n: tiebreak ? index - match.group_a.length + 1 : index + 1 },
  ];
}
