import { useTranslation } from '@broccoli/web-sdk/i18n';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { type ComponentProps, type ReactNode, useState } from 'react';

import { useNow } from './hooks/useNow';
import {
  deriveCountdownStatus,
  formatDurationMs,
  xiaojuTimingFromMatch,
} from './lib/countdown';
import { describeMatchPhase, type MatchStatusKind } from './lib/phase';
import type { MatchPhase, MatchView } from './types';

const PILL: Record<MatchStatusKind, string> = {
  not_started: 'bg-muted text-muted-foreground',
  ordering: 'bg-sky-500/10 text-sky-700 dark:text-sky-300',
  playing: 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300',
  awaiting_judge: 'bg-amber-500/15 text-amber-800 dark:text-amber-300',
  needs_adjudication: 'bg-red-500/10 text-red-700 dark:text-red-300',
  decided: 'bg-muted text-muted-foreground',
};

export function StatusPill({
  state,
  short,
  className,
}: {
  state: MatchPhase;
  /** Use the one- or two-word label, for tight spaces like bracket cards. */
  short?: boolean;
  className?: string;
}) {
  const { t } = useTranslation();
  const status = describeMatchPhase(state);
  return (
    <span
      className={cn(
        'inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-[11px] font-medium',
        PILL[status.kind],
        className,
      )}
    >
      {status.kind === 'playing' && (
        <span className="relative flex h-1.5 w-1.5">
          <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-500 opacity-60" />
          <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-emerald-500" />
        </span>
      )}
      {t(short ? status.shortKey : status.labelKey)}
    </span>
  );
}

/** mm:ss left in the current game, or null when no clock should show. */
export function useGameClock(match: MatchView): {
  text: string;
  overdue: boolean;
} | null {
  const now = useNow();
  const live =
    match.state === 'in_progress' ||
    match.state === 'tiebreak' ||
    match.state === 'awaiting_judge';
  if (!live) return null;
  const status = deriveCountdownStatus(xiaojuTimingFromMatch(match), now);
  if (status.kind === 'counting') {
    return { text: formatDurationMs(status.remainingMs), overdue: false };
  }
  if (status.kind === 'waiting-for-server') {
    return { text: `+${formatDurationMs(status.overdueMs)}`, overdue: true };
  }
  return null;
}

export function VerdictBadge({ verdict }: { verdict: string }) {
  const tone =
    verdict === 'Accepted'
      ? 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300'
      : /Pending|Queued|Compiling|Running|Judging/.test(verdict)
        ? 'bg-sky-500/10 text-sky-700 dark:text-sky-300'
        : verdict === 'SystemError'
          ? 'bg-amber-500/15 text-amber-800 dark:text-amber-300'
          : 'bg-red-500/10 text-red-700 dark:text-red-300';
  return (
    <span
      className={cn(
        'inline-flex rounded px-1.5 py-0.5 font-mono text-[11px] font-medium',
        tone,
      )}
    >
      {verdict}
    </span>
  );
}

/** A button that asks before doing something staff cannot take back. */
export function ConfirmButton({
  title,
  description,
  confirmLabel,
  onConfirm,
  children,
  ...buttonProps
}: {
  title: string;
  description: ReactNode;
  confirmLabel: string;
  onConfirm: () => void;
  children: ReactNode;
} & Omit<ComponentProps<typeof Button>, 'onClick'>) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button type="button" {...buttonProps} onClick={() => setOpen(true)}>
        {children}
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            <DialogDescription>{description}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              {t('codelink-bracket.confirm.cancel')}
            </Button>
            <Button
              onClick={() => {
                setOpen(false);
                onConfirm();
              }}
            >
              {confirmLabel}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
