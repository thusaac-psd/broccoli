import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { CONTEST_MANAGE } from '@broccoli/web-sdk/permissions';
import type { ReactNode } from 'react';

import { ContestEnrollCard } from '@/features/contest/components/ContestEnrollCard';
import { useContestEnroll } from '@/features/contest/hooks/use-contest-enroll';
import { useContestInfo } from '@/features/contest/hooks/use-contest-info';

/**
 * In a contest the viewer could register for but has not, what they can do
 * is register: they see that instead of `children` (the editor), rather than
 * an editor whose every submit the server refuses. Outside a contest
 * (`contestId` unset) it shows `children`.
 */
export function ContestRegisterGate({
  contestId,
  children,
}: {
  contestId?: number;
  children: ReactNode;
}) {
  if (contestId === undefined) return children;
  return <RegisterGate contestId={contestId}>{children}</RegisterGate>;
}

function RegisterGate({
  contestId,
  children,
}: {
  contestId: number;
  children: ReactNode;
}) {
  const { t } = useTranslation();
  const { user } = useAuth();
  const { contest } = useContestInfo(contestId);
  const enroll = useContestEnroll({
    contestId,
    contest,
    canManageContest: !!user?.permissions.includes(CONTEST_MANAGE),
  });

  if (!enroll.canShowEnrollCard) return children;
  return (
    <div className="mx-auto flex max-w-md flex-col gap-3 p-6">
      <p role="status" className="text-sm text-muted-foreground">
        {t('contests.registerToSubmit')}
      </p>
      <ContestEnrollCard
        onEnroll={enroll.enroll}
        isPending={enroll.isPending}
      />
    </div>
  );
}
