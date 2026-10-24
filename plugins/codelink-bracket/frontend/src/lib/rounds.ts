// Names count backwards from the final, independent of bracket size.
export const DEFAULT_PLAYER_COUNT = 16;

export function matchesInRound(round: number, stageCount: number): number {
  return 2 ** (stageCount - round);
}

export function roundNameKey(round: number, stageCount: number): string {
  const remaining = stageCount - round;
  const name =
    remaining === 0
      ? 'final'
      : remaining === 1
        ? 'semifinal'
        : remaining === 2
          ? 'quarterfinal'
          : `of${2 ** (remaining + 1)}`;
  return `codelink-bracket.round.${name}`;
}

export function roundShortKey(round: number, stageCount: number): string {
  return roundNameKey(round, stageCount).replace(
    'codelink-bracket.round.',
    'codelink-bracket.round.short.',
  );
}
import type { MatchView } from '../types.ts';

export function matchNameKey(
  match: Pick<MatchView, 'kind' | 'round' | 'stage_count'>,
): string {
  return match.kind === 'third_place'
    ? 'codelink-bracket.round.thirdPlace'
    : roundNameKey(match.round, match.stage_count);
}
