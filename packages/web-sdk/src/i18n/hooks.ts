import { use, useMemo } from 'react';

import { serverNow } from '@/api/server-clock';
import {
  type DateInput,
  type DateTimeOptions,
  formatDate,
  formatDateTime,
  formatLongDate,
  formatRelativeSeconds,
  formatRelativeTime,
  formatTime,
} from '@/i18n/format';
import { I18nContext } from '@/i18n/i18n-context';

export function useTranslation() {
  const context = use(I18nContext);
  if (!context) {
    throw new Error('useTranslation must be used within an I18nProvider');
  }
  return context;
}

/** The date formatters of `@broccoli/web-sdk/i18n`, bound to the UI locale. */
export interface Formatters {
  dateTime: (value: DateInput, options?: DateTimeOptions) => string;
  date: (value: DateInput) => string;
  longDate: (value: DateInput) => string;
  time: (value: DateInput) => string;
  /** A server timestamp relative to the server's clock, e.g. "3 minutes ago". */
  relative: (value: DateInput) => string;
  /** An offset from now in seconds (negative is past), e.g. "in 2 hours". */
  relativeSeconds: (offsetSeconds: number) => string;
}

export function useFormat(): Formatters {
  const { locale } = useTranslation();
  return useMemo(
    () => ({
      dateTime: (value, options) => formatDateTime(value, locale, options),
      date: (value) => formatDate(value, locale),
      longDate: (value) => formatLongDate(value, locale),
      time: (value) => formatTime(value, locale),
      relative: (value) => formatRelativeTime(value, serverNow(), locale),
      relativeSeconds: (offset) => formatRelativeSeconds(offset, locale),
    }),
    [locale],
  );
}
