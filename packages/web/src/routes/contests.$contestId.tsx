import { Slot } from '@broccoli/web-sdk/slot';
import { Outlet, useParams } from 'react-router';

import { ContestSlotProvider } from '@/features/contest/components/ContestSlotProvider';

export default function ContestLayout() {
  const { contestId } = useParams();
  const id = Number(contestId);

  if (!contestId || Number.isNaN(id)) {
    return <Outlet />;
  }

  return (
    <ContestSlotProvider contestId={id}>
      <Slot name="contest.layout">
        <Outlet />
      </Slot>
    </ContestSlotProvider>
  );
}
