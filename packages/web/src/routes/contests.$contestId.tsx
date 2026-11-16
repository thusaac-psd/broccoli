import { Slot } from '@broccoli/web-sdk/slot';
import { Outlet, useParams } from 'react-router';

import { ContestSlotProvider } from '@/features/contest/components/ContestSlotProvider';
import { contestPageTitle, type RouteHandle } from '@/lib/route-title';

export const handle: RouteHandle = contestPageTitle();

export default function ContestLayout() {
  const { contestId } = useParams();
  const id = Number(contestId);

  if (!contestId || Number.isNaN(id)) {
    return <Outlet />;
  }

  return (
    <ContestSlotProvider contestId={id}>
      <Slot name="contest.layout" className="flex min-h-0 flex-1 flex-col">
        <Outlet />
      </Slot>
    </ContestSlotProvider>
  );
}
