import { Button } from '@broccoli/web-sdk/ui';
import { cn } from '@broccoli/web-sdk/utils';
import { useState } from 'react';

import { canSubmitOrder } from './lib/ordering';
import type { MaskedTriple } from './types';

interface OrderingPanelProps {
  /** The OPPONENT's group of 3 problems, as this viewer sees it (masked). */
  opponentGroup: MaskedTriple;
  onSubmit: (order: [number, number, number]) => void;
  submitting: boolean;
  /** Set once this viewer has already submitted a ranking for this match. */
  alreadySubmitted: boolean;
}

/**
 * Drag-and-drop is not used here on purpose: a "click each problem once, in
 * the order you want your opponent to face them" build produces a ranking
 * that is a permutation OF THE VISIBLE PROBLEMS by construction -- there is
 * no reordering gesture that can leave a duplicate or omission in the
 * `picks` array. `canSubmitOrder` (shared with the tested pure-logic
 * module) is still consulted before enabling Submit, both as defense in
 * depth and because it is also the function that gates the case a
 * click-based UI cannot self-prevent: the opponent's group not being fully
 * visible yet (a masked entry), where there is nothing valid to rank at
 * all.
 */
export function OrderingPanel({
  opponentGroup,
  onSubmit,
  submitting,
  alreadySubmitted,
}: OrderingPanelProps) {
  const [picks, setPicks] = useState<number[]>([]);

  const hiddenEntryCount = opponentGroup.filter((id) => id === null).length;
  if (hiddenEntryCount > 0) {
    return (
      <div className="rounded-md border border-border bg-muted/40 p-3 text-sm text-muted-foreground">
        Waiting to see the opponent&apos;s problems before you can rank them.
      </div>
    );
  }

  const visibleProblems = opponentGroup.filter(
    (id): id is number => id !== null,
  );
  const remaining = visibleProblems.filter((id) => !picks.includes(id));
  const canSubmit = canSubmitOrder(picks, opponentGroup) && !submitting;

  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">
        {alreadySubmitted
          ? 'You already submitted a ranking. Submitting again replaces it until the match starts.'
          : "Click the opponent's 3 problems below, in the order you want them to face them (first click = 1st)."}
      </p>

      <div className="flex flex-wrap gap-2">
        {remaining.map((problemId) => (
          <button
            key={problemId}
            type="button"
            onClick={() => setPicks((prev) => [...prev, problemId])}
            className="rounded-md border border-input bg-background px-3 py-1.5 text-sm hover:bg-accent"
          >
            Problem {problemId}
          </button>
        ))}
      </div>

      <ol className="flex flex-col gap-1">
        {picks.map((problemId, index) => (
          <li
            key={problemId}
            className={cn(
              'flex items-center justify-between rounded-md border border-border px-3 py-1.5 text-sm',
            )}
          >
            <span>
              {index + 1}. Problem {problemId}
            </span>
            <button
              type="button"
              onClick={() =>
                setPicks((prev) => prev.filter((id) => id !== problemId))
              }
              className="text-xs text-muted-foreground hover:text-foreground"
            >
              Remove
            </button>
          </li>
        ))}
        {picks.length === 0 && (
          <li className="text-sm text-muted-foreground">
            No problems ranked yet.
          </li>
        )}
      </ol>

      <div className="flex items-center gap-2">
        <Button
          type="button"
          disabled={!canSubmit}
          onClick={() => {
            if (picks.length !== 3) return;
            const [first, second, third] = picks;
            if (
              first === undefined ||
              second === undefined ||
              third === undefined
            ) {
              return;
            }
            onSubmit([first, second, third]);
          }}
        >
          {submitting ? 'Submitting...' : 'Submit ranking'}
        </Button>
        {picks.length > 0 && (
          <Button
            type="button"
            variant="outline"
            onClick={() => setPicks([])}
            disabled={submitting}
          >
            Reset
          </Button>
        )}
      </div>
    </div>
  );
}
