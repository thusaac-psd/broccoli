// How a bracket player is named in the UI: the display name (or login name)
// resolved by the server (`MatchView.player_{a,b}_name`), else `#<user id>` -- the same
// fallback everywhere, so a failed name lookup never renders a blank.

export function playerLabel(
  name: string | null | undefined,
  id: number,
): string {
  return name?.trim() || `#${id}`;
}
