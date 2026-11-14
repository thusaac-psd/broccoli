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
