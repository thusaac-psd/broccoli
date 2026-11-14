// Dates, times and relative times for the UI locale. Every date the UI shows
// goes through these, so it reads the same way in every view and follows the
// language the user picked, not the browser's.

/** A timestamp: a `Date`, epoch milliseconds, or an ISO 8601 string. */
export type DateInput = Date | number | string;

export interface DateTimeOptions {
  /**
   * Name the time zone ("GMT+8"). Use it on absolute times people compare
   * across machines, such as contest start and end times.
   */
  timeZoneName?: boolean;
}

function toDate(value: DateInput): Date {
  return value instanceof Date ? value : new Date(value);
}

const formats = new Map<string, Intl.DateTimeFormat>();

function dateFormat(
  locale: string,
  options: Intl.DateTimeFormatOptions,
): Intl.DateTimeFormat {
  const key = `${locale}|${JSON.stringify(options)}`;
  let format = formats.get(key);
  if (!format) {
    format = new Intl.DateTimeFormat(locale, options);
    formats.set(key, format);
  }
  return format;
}

const DATE: Intl.DateTimeFormatOptions = {
  year: 'numeric',
  month: 'short',
  day: 'numeric',
};
const TIME: Intl.DateTimeFormatOptions = {
  hour: '2-digit',
  minute: '2-digit',
  hourCycle: 'h23',
};

/** Date and time, e.g. "Nov 14, 2026, 08:09" or "2026年11月14日 08:09". */
export function formatDateTime(
  value: DateInput,
  locale: string,
  options: DateTimeOptions = {},
): string {
  return dateFormat(locale, {
    ...DATE,
    ...TIME,
    ...(options.timeZoneName ? { timeZoneName: 'short' } : {}),
  }).format(toDate(value));
}

/** The calendar date alone, e.g. "Nov 14, 2026". */
export function formatDate(value: DateInput, locale: string): string {
  return dateFormat(locale, DATE).format(toDate(value));
}

/** A long calendar date, e.g. "November 14, 2026" or "2026年11月14日". */
export function formatLongDate(value: DateInput, locale: string): string {
  return dateFormat(locale, { ...DATE, month: 'long' }).format(toDate(value));
}

/** The time of day alone, e.g. "08:09". */
export function formatTime(value: DateInput, locale: string): string {
  return dateFormat(locale, TIME).format(toDate(value));
}

/** The month and year a calendar shows, e.g. "November 2026". */
export function formatMonthYear(value: DateInput, locale: string): string {
  return dateFormat(locale, { year: 'numeric', month: 'long' }).format(
    toDate(value),
  );
}

/** A short weekday name, e.g. "Mo" or "一". */
export function formatWeekday(value: DateInput, locale: string): string {
  return dateFormat(locale, { weekday: 'narrow' }).format(toDate(value));
}

const relativeFormats = new Map<string, Intl.RelativeTimeFormat>();

// The largest unit whose magnitude reaches the threshold is used; under 45
// seconds reads as "now".
const RELATIVE_UNITS: [Intl.RelativeTimeFormatUnit, number, number][] = [
  // unit, seconds per unit, the magnitude (in seconds) it starts at
  ['year', 365 * 86_400, 320 * 86_400],
  ['month', 30 * 86_400, 26 * 86_400],
  ['day', 86_400, 22 * 3_600],
  ['hour', 3_600, 45 * 60],
  ['minute', 60, 45],
];

/**
 * How far `offsetSeconds` lies from now, e.g. "3 minutes ago", "in 1 day",
 * "now" ("3分钟前", "明天", "现在"). Negative offsets are in the past.
 */
export function formatRelativeSeconds(
  offsetSeconds: number,
  locale: string,
): string {
  let format = relativeFormats.get(locale);
  if (!format) {
    format = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' });
    relativeFormats.set(locale, format);
  }
  const magnitude = Math.abs(offsetSeconds);
  for (const [unit, size, from] of RELATIVE_UNITS) {
    if (magnitude >= from) {
      return format.format(Math.round(offsetSeconds / size), unit);
    }
  }
  return format.format(0, 'second');
}

/**
 * How far `value` lies from `now` (epoch milliseconds). For a server
 * timestamp pass the server's clock (`serverNow()`), never `Date.now()`.
 */
export function formatRelativeTime(
  value: DateInput,
  now: number,
  locale: string,
): string {
  return formatRelativeSeconds((toDate(value).getTime() - now) / 1000, locale);
}
