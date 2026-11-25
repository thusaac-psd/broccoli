/** One choice of a submission list's filter (a language, a problem). */
export interface FilterOption {
  value: string;
  label: string;
}

/**
 * The choices of a submission list's filter: every choice the server lists
 * (its languages, a contest's problems) after "all". A filter applied from
 * the address stays a choice even when the list has not loaded, so the
 * dropdown names it, and a list that failed to load says so on its first
 * choice instead of reading as complete.
 */
export function filterOptions(
  labels: { all: string; allUnavailable: string },
  listed: readonly FilterOption[] | undefined,
  failed: boolean,
  applied: string | null,
): FilterOption[] {
  const known = listed ?? [];
  const keepsApplied =
    applied !== null && !known.some((option) => option.value === applied);
  return [
    { value: 'all', label: failed ? labels.allUnavailable : labels.all },
    ...known,
    ...(keepsApplied ? [{ value: applied, label: applied }] : []),
  ];
}
