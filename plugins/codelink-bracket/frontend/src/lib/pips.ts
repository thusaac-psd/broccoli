// One marker per game for a player's row on a bracket card, so a projected
// bracket shows how each match went without opening it.

import type { MatchView } from '../types.ts';

export type Pip = 'won' | 'lost' | 'void' | 'live' | 'upcoming';

/**
 * The three regular games, then any tiebreaks that have opened. `void` is a
 * decided game nobody won; `live` is the open game; `upcoming` has not
 * opened yet (never shown once the match is decided).
 */
export function gamePips(match: MatchView, playerId: number): Pip[] {
  const byIndex = new Map(match.games.map((g) => [g.index, g]));
  const lastIndex = Math.max(2, ...match.games.map((g) => g.index));
  const pips: Pip[] = [];
  for (let i = 0; i <= lastIndex; i++) {
    const g = byIndex.get(i);
    if (!g) {
      if (match.state !== 'decided') pips.push('upcoming');
      continue;
    }
    // A game left open when staff decided the match (award, or a stuck
    // judge) never finished: nobody scored it, and it is not live.
    const matchOver =
      match.state === 'decided' || match.state === 'needs_adjudication';
    if (!g.decided) pips.push(matchOver ? 'void' : 'live');
    else if (g.winner === null) pips.push('void');
    else pips.push(g.winner === playerId ? 'won' : 'lost');
  }
  return pips;
}

/** First-round seed per player: match `pos` holds seeds 2*pos+1 and 2*pos+2. */
export function seedsFrom(matches: MatchView[]): Map<number, number> {
  const seeds = new Map<number, number>();
  for (const m of matches) {
    if (m.round !== 1) continue;
    seeds.set(m.player_a, 2 * m.pos + 1);
    seeds.set(m.player_b, 2 * m.pos + 2);
  }
  return seeds;
}
