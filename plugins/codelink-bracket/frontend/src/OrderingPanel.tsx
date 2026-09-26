import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Button } from '@broccoli/web-sdk/ui';
import { ExternalLink, Lock } from 'lucide-react';
import { useState } from 'react';
import { Link } from 'react-router';

import type { ProblemInfo } from './hooks/useContestData';
import { problemPath } from './lib/links';
import { canSubmitOrder } from './lib/ordering';
import { SortableList } from './SortableList';
import type { MaskedTriple } from './types';

interface OrderingPanelProps {
  contestId: number;
  /** The OPPONENT's group of 3 problems, as this viewer sees it (masked). */
  opponentGroup: MaskedTriple;
  /** This viewer's earlier ranking, if they already sent one. */
  submittedOrder: MaskedTriple | null;
  problems: ReadonlyMap<number, ProblemInfo>;
  onSubmit: (order: [number, number, number]) => void;
  submitting: boolean;
}

/**
 * The player ranks the opponent's three problems by dragging them into the
 * order the opponent must solve them. The list always holds exactly the
 * visible group, so any arrangement is a valid permutation; `canSubmitOrder`
 * is still checked, and also covers the one case dragging cannot: part of the
 * group not being visible yet.
 */
export function OrderingPanel({
  contestId,
  opponentGroup,
  submittedOrder,
  problems,
  onSubmit,
  submitting,
}: OrderingPanelProps) {
  const { t } = useTranslation();
  const visible = opponentGroup.filter((id): id is number => id !== null);
  const initial =
    submittedOrder && submittedOrder.every((id) => id !== null)
      ? (submittedOrder as number[])
      : visible;
  const [order, setOrder] = useState<number[]>(initial);

  if (visible.length < opponentGroup.length) {
    return (
      <p className="rounded-lg border border-dashed border-border p-4 text-sm text-muted-foreground">
        {t('codelink-bracket.ordering.hidden')}
      </p>
    );
  }

  const alreadySubmitted = submittedOrder !== null;
  const canSubmit = canSubmitOrder(order, opponentGroup) && !submitting;

  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">
        {t('codelink-bracket.ordering.instructions')}
      </p>
      <SortableList
        items={order}
        getKey={(id) => id}
        onChange={setOrder}
        disabled={submitting}
        renderItem={(id) => {
          const p = problems.get(id);
          return (
            <div className="flex items-center justify-between gap-2">
              <span className="truncate">
                <span className="font-mono font-semibold">
                  {p?.label ?? `#${id}`}
                </span>
                {p && (
                  <span className="ml-2 text-muted-foreground">{p.title}</span>
                )}
              </span>
              <Link
                to={problemPath(contestId, id)}
                target="_blank"
                onClick={(e) => e.stopPropagation()}
                className="shrink-0 text-muted-foreground hover:text-foreground"
                aria-label={t('codelink-bracket.ordering.openProblem')}
              >
                <ExternalLink className="h-3.5 w-3.5" />
              </Link>
            </div>
          );
        }}
      />
      <div className="flex items-center justify-between gap-3">
        <p className="text-xs text-muted-foreground">
          {alreadySubmitted
            ? t('codelink-bracket.ordering.resubmit')
            : t('codelink-bracket.ordering.hint')}
        </p>
        <Button
          type="button"
          disabled={!canSubmit}
          onClick={() => {
            const [first, second, third] = order;
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
          <Lock className="mr-1.5 h-3.5 w-3.5" />
          {submitting
            ? t('codelink-bracket.ordering.submitting')
            : alreadySubmitted
              ? t('codelink-bracket.ordering.update')
              : t('codelink-bracket.ordering.submit')}
        </Button>
      </div>
    </div>
  );
}
