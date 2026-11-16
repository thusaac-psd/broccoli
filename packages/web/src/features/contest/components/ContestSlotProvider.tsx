import { ContestSlotContext } from '@broccoli/web-sdk/slot';
import { type ReactNode, useMemo } from 'react';

import { useContestInfo } from '@/features/contest/hooks/use-contest-info';

interface ContestSlotProviderProps {
  contestId: number;
  children: ReactNode;
}

/**
 * Populates the ContestSlotContext with the current contest's id, type and
 * contest record, so the SDK's <Slot> component can filter plugin slot entries
 * declared with a `contest_type` (e.g. only render IOI slots on IOI contests),
 * and a plugin view of the contest can pace itself on the contest's phase.
 *
 * While the contest info is still loading, we render children with a null
 * contestType - slots with a non-empty contest_type will be skipped until the
 * real type is known. This avoids briefly mounting an IOI scoreboard on an
 * ICPC contest while the contest fetch is in flight.
 */
export function ContestSlotProvider({
  contestId,
  children,
}: ContestSlotProviderProps) {
  const { contest } = useContestInfo(contestId);

  const value = useMemo(
    () => ({
      contestId,
      contestType: contest?.contest_type ?? null,
      contest: contest ?? null,
    }),
    [contestId, contest],
  );

  return <ContestSlotContext value={value}>{children}</ContestSlotContext>;
}
