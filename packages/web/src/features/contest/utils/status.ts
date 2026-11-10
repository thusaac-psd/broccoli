import { serverNow } from '@broccoli/web-sdk/api';
import type { BadgeProps } from '@broccoli/web-sdk/ui';

export function getContestStatus(
  startTime: string,
  endTime: string,
  t: (key: string) => string,
): { label: string; variant: NonNullable<BadgeProps['variant']> } {
  const now = serverNow();
  const start = Date.parse(startTime);
  const end = Date.parse(endTime);

  if (now < start) return { label: t('contests.upcoming'), variant: 'outline' };
  if (now <= end) return { label: t('contests.running'), variant: 'default' };
  return { label: t('contests.ended'), variant: 'secondary' };
}
