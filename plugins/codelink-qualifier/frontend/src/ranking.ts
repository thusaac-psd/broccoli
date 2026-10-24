import type { Standing } from './types';

export type RankingMode = 'points' | 'accepted';

export function rankStandings(rows: readonly Standing[], mode: RankingMode) {
  const ordered =
    mode === 'accepted'
      ? [...rows].sort(
          (a, b) => b.accepted - a.accepted || a.user_id - b.user_id,
        )
      : rows;
  let rank = 0;
  return ordered.map((row, index) => {
    if (
      index === 0 ||
      mode === 'points' ||
      row.accepted !== ordered[index - 1].accepted
    ) {
      rank = index + 1;
    }
    return { row, rank };
  });
}
