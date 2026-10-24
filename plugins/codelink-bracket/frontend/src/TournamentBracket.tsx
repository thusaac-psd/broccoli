import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { useEffect, useMemo, useRef, useState } from 'react';

import { BracketMatchCard } from './BracketMatchCard';
import { seedsFrom } from './lib/pips';
import { playerLabel } from './lib/player';
import { roundNameKey, roundShortKey } from './lib/rounds';
import { sideOf } from './lib/stage';
import {
  type BracketNode,
  nodeKey,
  tournamentLayout,
} from './lib/tournament-layout';
import type { MatchView } from './types';

export function TournamentBracket({
  players,
  thirdPlace,
  matches,
  viewerId,
  onOpen,
}: {
  players: number;
  thirdPlace: boolean;
  matches: MatchView[];
  viewerId: number | null;
  onOpen: (id: number) => void;
}) {
  const { t } = useTranslation();
  const layout = useMemo(
    () => tournamentLayout(players, thirdPlace),
    [players, thirdPlace],
  );
  const at = new Map(
    matches.map((match) => [`${match.round}:${match.pos}`, match]),
  );
  const seeds = seedsFrom(matches);
  const canvas = useRef<HTMLDivElement>(null);
  const [fit, setFit] = useState(false);
  const [availableWidth, setAvailableWidth] = useState(0);
  const scale =
    fit && availableWidth ? Math.min(1, availableWidth / layout.width) : 1;
  useEffect(() => {
    const element = canvas.current;
    if (!element) return;
    const observer = new ResizeObserver(() =>
      setAvailableWidth(element.clientWidth),
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    const element = canvas.current;
    if (element)
      element.scrollLeft = Math.max(
        0,
        (layout.width * scale - element.clientWidth) / 2,
      );
  }, [layout.width, scale]);

  const feederName = (node: BracketNode, side: number) => {
    if (node.round === 1)
      return t('codelink-bracket.slot.seed', { seed: 2 * node.pos + side + 1 });
    const sourcePos = node.kind === 'third_place' ? side : 2 * node.pos + side;
    const source = at.get(`${node.round - 1}:${sourcePos}`);
    if (source?.state === 'decided' && source.winner !== null) {
      const id =
        node.kind === 'third_place'
          ? source.winner === source.player_a
            ? source.player_b
            : source.player_a
          : source.winner;
      return playerLabel(
        id === source.player_a ? source.player_a_name : source.player_b_name,
        id,
      );
    }
    return t(
      `codelink-bracket.slot.${node.kind === 'third_place' ? 'loserOf' : 'winnerOf'}`,
      {
        match: `${t(roundShortKey(node.round - 1, layout.stages))} ${sourcePos + 1}`,
      },
    );
  };

  return (
    <div className="space-y-2">
      <div className="flex justify-end">
        <Button size="sm" variant="ghost" onClick={() => setFit(!fit)}>
          {t(`codelink-bracket.tree.${fit ? 'actualSize' : 'fitWidth'}`)}
        </Button>
      </div>
      <div
        ref={canvas}
        className="overflow-x-auto overscroll-x-contain pb-4"
        aria-label={t('codelink-bracket.tree.label')}
      >
        <div
          style={{
            width: layout.width * scale,
            height: (layout.height + 36) * scale,
          }}
        >
          <div
            style={{
              width: layout.width,
              transform: `scale(${scale})`,
              transformOrigin: 'top left',
            }}
          >
            <div className="relative mb-3 h-6">
              {layout.headings.map((heading) => (
                <div
                  key={`${heading.round}:${heading.x}`}
                  className="absolute text-center text-sm font-medium text-muted-foreground"
                  style={{ left: heading.x, width: heading.width }}
                >
                  {t(
                    heading.round === layout.stages && thirdPlace
                      ? 'codelink-bracket.round.finals'
                      : roundNameKey(heading.round, layout.stages),
                  )}
                </div>
              ))}
            </div>
            <div
              className="relative"
              style={{ width: layout.width, height: layout.height }}
            >
              <svg
                className="pointer-events-none absolute inset-0 h-full w-full overflow-visible text-border"
                viewBox={`0 0 ${layout.width} ${layout.height}`}
                aria-hidden
              >
                {layout.edges.map((edge) => (
                  <path
                    key={`${edge.from}:${edge.to}`}
                    d={edge.path}
                    fill="none"
                    stroke="currentColor"
                    strokeWidth={2}
                    strokeDasharray={
                      edge.outcome === 'loser' ? '5 4' : undefined
                    }
                  />
                ))}
              </svg>
              {layout.nodes.map((node) => {
                const match = at.get(nodeKey(node));
                const central = node.side === 'center';
                return (
                  <div
                    key={nodeKey(node)}
                    className="absolute"
                    style={{
                      left: node.x,
                      top: node.y,
                      width: node.width,
                      height: node.height,
                    }}
                  >
                    {central && (
                      <div className="absolute -top-8 left-0 right-0 text-center text-sm font-semibold">
                        {t(
                          `codelink-bracket.round.${node.kind === 'third_place' ? 'thirdPlace' : 'final'}`,
                        )}
                      </div>
                    )}
                    {match ? (
                      <BracketMatchCard
                        match={match}
                        seeds={seeds}
                        isMine={sideOf(match, viewerId) !== null}
                        horizontal={central}
                        onOpen={() => onOpen(match.id)}
                      />
                    ) : (
                      <div
                        className={`flex h-full w-full ${central ? 'flex-row divide-x' : 'flex-col divide-y'} divide-border rounded-lg border border-dashed border-border bg-card text-xs text-muted-foreground`}
                      >
                        {[0, 1].map((side) => (
                          <div
                            key={side}
                            className="flex min-w-0 flex-1 items-center justify-center px-3 py-2 text-center"
                          >
                            {feederName(node, side)}
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
