import { serverNow } from '@/api/server-clock';

export function formatTime(isoString: string, locale?: string) {
  return new Date(isoString).toLocaleTimeString(locale, {
    hour: '2-digit',
    minute: '2-digit',
  });
}

export function formatDateTime(dateStr: string, locale?: string): string {
  const validLocale = locale && locale !== 'undefined' ? locale : 'en-US';
  return new Date(dateStr).toLocaleString(validLocale, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

export function formatRelativeDatetime(
  dateStr: string,
  t: (key: string, params?: Record<string, string>) => string,
): string {
  // Server-stamped times, so measured against the server's clock.
  const diffMs = Date.parse(dateStr) - serverNow();
  const absDiffMs = Math.abs(diffMs);
  const mins = Math.floor(absDiffMs / 60000);
  const hours = Math.floor(mins / 60);
  const days = Math.floor(hours / 24);

  // TODO: rename these translation keys to "common.time.*"
  if (diffMs > 0) {
    if (days > 0) return t('contests.inDays', { count: String(days) });
    if (hours > 0) return t('contests.inHours', { count: String(hours) });
    return t('contests.inMinutes', { count: String(mins) });
  }
  if (days > 0) return t('contests.daysAgo', { count: String(days) });
  if (hours > 0) return t('contests.hoursAgo', { count: String(hours) });
  return t('contests.minutesAgo', { count: String(mins) });
}
