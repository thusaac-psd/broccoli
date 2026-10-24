import type { MatchKind } from '../types.ts';

export interface BracketNode {
  round: number;
  pos: number;
  kind: MatchKind;
  side: 'left' | 'right' | 'center';
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface BracketEdge {
  from: string;
  to: string;
  outcome: 'winner' | 'loser';
  path: string;
}

const CARD_WIDTH = 216;
const CARD_HEIGHT = 124;
const COLUMN_GAP = 36;
const ROW_HEIGHT = 156;
const CENTER_WIDTH = 372;

export const nodeKey = (node: Pick<BracketNode, 'round' | 'pos'>) =>
  `${node.round}:${node.pos}`;

/** Two fixed halves feed the final and bronze match at the four central ports. */
export function tournamentLayout(players: number, thirdPlace: boolean) {
  const stages = Math.log2(players);
  if (players < 2 || players > 128 || !Number.isInteger(stages)) {
    throw new RangeError('Bracket size must be a power of two from 2 to 128');
  }
  const bronze = thirdPlace && players >= 4;
  const wingColumns = stages - 1;
  const centerX = wingColumns * (CARD_WIDTH + COLUMN_GAP);
  const width = centerX * 2 + CENTER_WIDTH;
  const rows = Math.max(1, players / 4);
  const height = Math.max(rows * ROW_HEIGHT, bronze ? 420 : 180);
  const rowHeight = height / rows;
  const nodes: BracketNode[] = [];
  const headings: { round: number; x: number; width: number }[] = [];
  for (let round = 1; round < stages; round++) {
    const perSide = 2 ** (stages - round - 1);
    const span = 2 ** (round - 1);
    for (const side of ['left', 'right'] as const) {
      const x =
        side === 'left'
          ? (round - 1) * (CARD_WIDTH + COLUMN_GAP)
          : width - CARD_WIDTH - (round - 1) * (CARD_WIDTH + COLUMN_GAP);
      headings.push({ round, x, width: CARD_WIDTH });
      for (let local = 0; local < perSide; local++) {
        nodes.push({
          round,
          pos: local + (side === 'right' ? perSide : 0),
          kind: 'elimination',
          side,
          x,
          y: (local + 0.5) * span * rowHeight - CARD_HEIGHT / 2,
          width: CARD_WIDTH,
          height: CARD_HEIGHT,
        });
      }
    }
  }
  headings.push({ round: stages, x: centerX, width: CENTER_WIDTH });
  nodes.push({
    round: stages,
    pos: 0,
    kind: 'final',
    side: 'center',
    x: centerX,
    y: height / 2 - CARD_HEIGHT / 2 - (bronze ? 112 : 0),
    width: CENTER_WIDTH,
    height: CARD_HEIGHT,
  });
  if (bronze)
    nodes.push({
      round: stages,
      pos: 1,
      kind: 'third_place',
      side: 'center',
      x: centerX,
      y: height / 2 - CARD_HEIGHT / 2 + 112,
      width: CENTER_WIDTH,
      height: CARD_HEIGHT,
    });
  const byKey = new Map(nodes.map((node) => [nodeKey(node), node]));
  const edges: BracketEdge[] = [];
  for (const from of nodes.filter((node) => node.side !== 'center')) {
    const targets =
      from.round === stages - 1
        ? [
            { pos: 0, outcome: 'winner' as const },
            ...(bronze ? [{ pos: 1, outcome: 'loser' as const }] : []),
          ]
        : [{ pos: Math.floor(from.pos / 2), outcome: 'winner' as const }];
    for (const { pos, outcome } of targets) {
      const to = byKey.get(`${from.round + 1}:${pos}`)!;
      const x1 = from.side === 'left' ? from.x + from.width : from.x;
      const x2 = from.side === 'left' ? to.x : to.x + to.width;
      const y1 = from.y + from.height / 2;
      const y2 = to.y + to.height / 2;
      const elbow = (x1 + x2) / 2;
      edges.push({
        from: nodeKey(from),
        to: nodeKey(to),
        outcome,
        path: `M ${x1} ${y1} H ${elbow} V ${y2} H ${x2}`,
      });
    }
  }
  return { stages, width, height, nodes, edges, headings };
}
