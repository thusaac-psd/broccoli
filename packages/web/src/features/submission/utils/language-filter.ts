/** One choice of a submission list's language filter. */
export interface FilterOption {
  value: string;
  label: string;
}

/**
 * The choices of a submission list's language filter: every language the
 * server supports after "all". A filter applied from the address stays a
 * choice even when the list has not loaded, so the dropdown names it, and a
 * list that failed to load says so on its first choice instead of reading as
 * complete.
 */
export function languageFilterOptions(
  labels: { all: string; allUnavailable: string },
  languages: readonly { id: string; name: string }[] | undefined,
  failed: boolean,
  applied: string | null,
): FilterOption[] {
  const known = (languages ?? []).map((l) => ({ value: l.id, label: l.name }));
  const keepsApplied =
    applied !== null && !known.some((option) => option.value === applied);
  return [
    { value: 'all', label: failed ? labels.allUnavailable : labels.all },
    ...known,
    ...(keepsApplied ? [{ value: applied, label: applied }] : []),
  ];
}
