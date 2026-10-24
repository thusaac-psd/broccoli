import type { MatchView } from '../types.ts';
import type { Side } from './stage.ts';

/** Mirrors the server's current-game visibility for immediate UI transitions. */
export function matchPresentation(
  match: MatchView | undefined,
  side: Side | undefined,
) {
  if (!match || !side)
    return { hideLabels: true, allowedProblemIds: [] as number[] };
  if (match.state === 'ordering') {
    return {
      hideLabels: false,
      allowedProblemIds: (side === 'a' ? match.group_b : match.group_a).filter(
        (id): id is number => id !== null,
      ),
    };
  }
  const live = ['in_progress', 'tiebreak', 'awaiting_judge'].includes(
    match.state,
  );
  const game = match.games.find(
    (game) => game.index === match.current_xiaoju_index,
  );
  const problem =
    live && game ? (side === 'a' ? game.problem_a : game.problem_b) : null;
  return {
    hideLabels: true,
    allowedProblemIds: problem === null ? [] : [problem],
  };
}
