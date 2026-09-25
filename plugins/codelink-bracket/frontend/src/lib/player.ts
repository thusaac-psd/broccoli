// How a bracket player is named in the UI: their username when the server
// resolved one (`MatchView.player_{a,b}_name`), else `#<user id>` -- the same
// fallback everywhere, so a failed name lookup never renders a blank.

export function playerLabel(name: string | null, id: number): string {
  return name ?? `#${id}`;
}
