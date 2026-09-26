// Round naming and the slot layout of a 16-player single-elimination bracket.

export const ROUND_COUNT = 4;

/** Matches in `round` (1-based): 8, 4, 2, 1. */
export function matchesInRound(round: number): number {
  return 16 >> round;
}

/** Translation key for a round's name, counted back from the final. */
export function roundNameKey(round: number): string {
  switch (ROUND_COUNT - round) {
    case 0:
      return 'codelink-bracket.round.final';
    case 1:
      return 'codelink-bracket.round.semifinal';
    case 2:
      return 'codelink-bracket.round.quarterfinal';
    default:
      return 'codelink-bracket.round.of16';
  }
}

/** Translation key for a round's short tag (R16, QF, SF, F). */
export function roundShortKey(round: number): string {
  return roundNameKey(round).replace(
    'codelink-bracket.round.',
    'codelink-bracket.round.short.',
  );
}
