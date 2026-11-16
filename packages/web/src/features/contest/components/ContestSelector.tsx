import type { ContestSummary } from '@broccoli/web-sdk/contest';
import { useFormat, useTranslation } from '@broccoli/web-sdk/i18n';
import { Badge } from '@broccoli/web-sdk/ui';
import { CalendarDays, ChevronRight, Trophy } from 'lucide-react';
import { useNavigate } from 'react-router';

import { getContestStatus } from '@/features/contest/utils/status';

export function ContestSelector({ contests }: { contests: ContestSummary[] }) {
  const { t } = useTranslation();
  const format = useFormat();
  const navigate = useNavigate();

  return (
    <div>
      <h2 className="text-lg font-semibold mb-3">
        {t('homepage.selectContest')}
      </h2>
      <p className="text-sm text-muted-foreground mb-4">
        {t('homepage.selectContestDesc')}
      </p>
      <div className="space-y-2">
        {contests.map((contest) => {
          const { label, variant } = getContestStatus(contest, t);
          return (
            <button
              key={contest.id}
              onClick={() => navigate(`/contests/${contest.id}`)}
              className="group flex w-full items-center gap-4 rounded-lg border p-4 text-left transition-colors hover:bg-muted/50"
            >
              <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-primary/10">
                <Trophy className="h-5 w-5 text-primary" />
              </div>
              <div className="flex-1 min-w-0">
                <div className="font-medium">{contest.title}</div>
                <div className="text-xs text-muted-foreground mt-0.5 flex items-center gap-1.5">
                  <CalendarDays className="h-3 w-3" />
                  {format.dateTime(contest.start_time, {
                    timeZoneName: true,
                  })}{' '}
                  — {format.dateTime(contest.end_time, { timeZoneName: true })}
                </div>
              </div>
              <Badge variant={variant} className="shrink-0">
                {label}
              </Badge>
              <ChevronRight className="h-4 w-4 text-muted-foreground group-hover:text-primary transition-colors shrink-0" />
            </button>
          );
        })}
      </div>
    </div>
  );
}
