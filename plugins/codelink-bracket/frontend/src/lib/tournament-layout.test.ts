import assert from 'node:assert/strict';
import { test } from 'node:test';

import { nodeKey, tournamentLayout } from './tournament-layout.ts';

test('sixteen players occupy mirrored halves with four championship and bronze feeders', () => {
  const layout = tournamentLayout(16, true);
  assert.equal(layout.nodes.length, 16);
  const at = new Map(layout.nodes.map((node) => [nodeKey(node), node]));
  assert.equal(
    layout.nodes.filter((n) => n.round === 1 && n.side === 'left').length,
    4,
  );
  assert.equal(
    layout.nodes.filter((n) => n.round === 1 && n.side === 'right').length,
    4,
  );
  for (const left of layout.nodes.filter((n) => n.side === 'left')) {
    const perSide = 2 ** (layout.stages - left.round - 1);
    const right = at.get(`${left.round}:${left.pos + perSide}`)!;
    assert.equal(left.x + right.x + right.width, layout.width);
    assert.equal(left.y, right.y);
  }
  assert.deepEqual(
    layout.edges
      .filter((e) => e.to.startsWith('4:'))
      .map((e) => [e.from, e.to, e.outcome]),
    [
      ['3:0', '4:0', 'winner'],
      ['3:0', '4:1', 'loser'],
      ['3:1', '4:0', 'winner'],
      ['3:1', '4:1', 'loser'],
    ],
  );
  assert.equal(at.get('4:0')!.x, at.get('4:1')!.x);
  assert.ok(at.get('4:0')!.y < at.get('4:1')!.y);
});

test('all supported sizes have unique positions and correctly linked feeders', () => {
  for (const count of [2, 4, 8, 16, 32, 64, 128]) {
    for (const bronze of [false, true]) {
      const layout = tournamentLayout(count, bronze);
      const keys = new Set(layout.nodes.map(nodeKey));
      assert.equal(keys.size, count - 1 + Number(bronze && count >= 4));
      for (const edge of layout.edges) {
        assert.ok(keys.has(edge.from) && keys.has(edge.to));
      }
      for (const node of layout.nodes) {
        assert.ok(node.x >= 0 && node.x + node.width <= layout.width);
        assert.ok(node.y >= 0 && node.y + node.height <= layout.height);
      }
    }
  }
});
