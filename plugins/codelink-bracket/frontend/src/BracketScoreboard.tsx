import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { CONTEST_MANAGE } from '@broccoli/web-sdk/permissions';
import { Button, Skeleton } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { AlertTriangle, Clock, Maximize2, Minimize2 } from 'lucide-react';
import {
  type ReactNode,
  type RefObject,
  useEffect,
  useRef,
  useState,
} from 'react';

import { useBracket } from './hooks/useBracket';
import { useNow } from './hooks/useNow';
import { roundNameKey } from './lib/rounds';
import { sideOf } from './lib/stage';
import { contestEndWarning, currentRoundProgress } from './lib/summary';
import { MatchSheet } from './MatchSheet';
import { MyMatchStrip } from './MyMatchStrip';
import { TournamentBracket } from './TournamentBracket';
import type { MatchView } from './types';

interface BracketScoreboardProps {
  contestId?: number;
  contestType?: string;
  children?: ReactNode;
}

const needsAttention = (m: MatchView) =>
  m.state === 'awaiting_judge' || m.state === 'needs_adjudication';

/**
 * The ranking page for the afternoon round: the viewer's own match first (for
 * players), then the whole bracket as a tree. Registered at `ranking.content`.
 * Every match here is already visibility-filtered by `GET /bracket`.
 */
export function BracketScoreboard({
  contestId,
  children,
}: BracketScoreboardProps) {
  const { t } = useTranslation();
  const auth = useAuth();
  const [openMatch, setOpenMatch] = useState<number | null>(null);
  const presentRef = useRef<HTMLDivElement>(null);
  const presenting = useFullscreen(presentRef);

  const { data, isLoading, isError } = useBracket(contestId);

  if (!contestId) return <>{children}</>;

  if (isError) {
    return (
      <div className="rounded-lg border border-red-500/30 bg-red-500/5 p-6 text-center text-sm text-red-700">
        {t('codelink-bracket.bracket.loadError')}
      </div>
    );
  }
  if (isLoading || !data) {
    return (
      <div className="grid grid-cols-4 gap-8">
        {Array.from({ length: 4 }, (_, i) => (
          <Skeleton key={i} className="h-72 w-full" />
        ))}
      </div>
    );
  }

  const viewerId = auth.user?.id ?? null;
  const isStaff = (auth.user?.permissions ?? []).includes(CONTEST_MANAGE);

  if (!data.player_count) {
    return (
      <div className="rounded-lg border border-dashed border-border py-16 text-center text-muted-foreground">
        {t('codelink-bracket.bracket.empty')}
      </div>
    );
  }

  const matches = data.matches;
  const mine = matches
    .filter((m) => sideOf(m, viewerId) !== null)
    .sort((a, b) => b.round - a.round)[0];
  const mySide = mine ? sideOf(mine, viewerId) : null;

  return (
    <div className="space-y-4">
      {mine && mySide && (
        <MyMatchStrip contestId={contestId} match={mine} side={mySide} />
      )}
      <div
        ref={presentRef}
        className={cn(
          'space-y-3',
          presenting && 'overflow-auto bg-background p-6',
        )}
      >
        <SummaryBar
          matches={matches}
          stageCount={Math.log2(data.player_count)}
          thirdPlace={data.third_place_enabled ?? false}
          contestEndMs={data.contest_end_ms}
          isStaff={isStaff}
          presenting={presenting}
          onTogglePresent={() =>
            presenting
              ? void document.exitFullscreen()
              : void presentRef.current?.requestFullscreen()
          }
        />
        <TournamentBracket
          players={data.player_count}
          thirdPlace={data.third_place_enabled ?? false}
          matches={matches}
          viewerId={presenting ? null : viewerId}
          onOpen={setOpenMatch}
        />
      </div>
      <MatchSheet
        contestId={contestId}
        matchId={openMatch}
        onClose={() => setOpenMatch(null)}
        container={presenting ? presentRef.current : undefined}
        presentation={presenting}
      />
    </div>
  );
}

/** Whether `ref`'s element is currently the fullscreen element. */
function useFullscreen(ref: RefObject<HTMLElement | null>): boolean {
  const [on, setOn] = useState(false);
  useEffect(() => {
    const sync = () => setOn(document.fullscreenElement === ref.current);
    document.addEventListener('fullscreenchange', sync);
    return () => document.removeEventListener('fullscreenchange', sync);
  }, [ref]);
  return on;
}

function SummaryBar({
  matches,
  stageCount,
  thirdPlace,
  contestEndMs,
  isStaff,
  presenting,
  onTogglePresent,
}: {
  matches: MatchView[];
  stageCount: number;
  thirdPlace: boolean;
  contestEndMs?: number | null;
  isStaff: boolean;
  presenting: boolean;
  onTogglePresent: () => void;
}) {
  const { t } = useTranslation();
  const now = useNow(15_000);
  const current = currentRoundProgress(matches, stageCount, thirdPlace);
  const endWarning = contestEndWarning(
    matches,
    contestEndMs,
    now,
    stageCount,
    thirdPlace,
  );
  const attention = matches.filter(needsAttention).length;
  return (
    <div className="flex flex-wrap items-center justify-between gap-3 text-sm">
      <div className="flex items-center gap-2">
        <span className="font-semibold">
          {current
            ? t(
                current.round === stageCount && thirdPlace
                  ? 'codelink-bracket.round.finals'
                  : roundNameKey(current.round, stageCount),
              )
            : t('codelink-bracket.summary.finished')}
        </span>
        {current && (
          <>
            <span className="text-muted-foreground">·</span>
            <span className="text-muted-foreground">
              {t('codelink-bracket.summary.remaining', {
                count: current.remaining,
              })}
            </span>
          </>
        )}
      </div>
      <div className="ml-auto flex items-center gap-3">
        {isStaff && !presenting && endWarning && (
          <div className="flex items-center gap-2 rounded-lg bg-amber-500/10 px-3 py-1.5 text-sm font-medium text-amber-800 dark:text-amber-200">
            <Clock className="h-4 w-4" />
            {endWarning.kind === 'ended'
              ? t('codelink-bracket.summary.contestEnded', {
                  count: endWarning.unfinished,
                })
              : t('codelink-bracket.summary.contestEndCutoff', {
                  count: endWarning.count,
                })}
          </div>
        )}
        {/* Staff-only signal: kept off the projected view. */}
        {isStaff && !presenting && attention > 0 && (
          <div className="flex items-center gap-2 rounded-lg bg-red-500/10 px-3 py-1.5 text-sm font-medium text-red-700 dark:text-red-300">
            <AlertTriangle className="h-4 w-4" />
            {t('codelink-bracket.summary.attention', { count: attention })}
          </div>
        )}
        <Button size="sm" variant="outline" onClick={onTogglePresent}>
          {presenting ? (
            <Minimize2 className="mr-1.5 h-3.5 w-3.5" />
          ) : (
            <Maximize2 className="mr-1.5 h-3.5 w-3.5" />
          )}
          {presenting
            ? t('codelink-bracket.summary.exitPresent')
            : t('codelink-bracket.summary.present')}
        </Button>
      </div>
    </div>
  );
}
