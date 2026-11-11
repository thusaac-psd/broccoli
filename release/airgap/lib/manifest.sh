#!/usr/bin/env bash
# Manifest generate/verify shared by build-bundle.sh (staging) and
# load-bundle.sh (target). sha256 over every bundle file except the
# manifest itself; paths are relative to the bundle root and sorted so
# the manifest is reproducible.
set -euo pipefail

# Portable sha256: coreutils `sha256sum` (Linux) or `shasum -a 256` (macOS). Both
# emit identical "<hex>  <path>" lines, so a manifest written with one
# cross-verifies under the other — a macOS operator can check a Linux-built
# bundle. Kept as an array so it expands into xargs as the command plus its args.
# Air-gap safe: both are base-OS tools, no network.
if command -v sha256sum >/dev/null 2>&1; then
  _MANIFEST_SHA256=(sha256sum)
elif command -v shasum >/dev/null 2>&1; then
  _MANIFEST_SHA256=(shasum -a 256)
else
  echo "manifest: need sha256sum or shasum -a 256 (neither found)" >&2
  return 1 2>/dev/null || exit 1
fi

# NOTE: .env.infra/.env.server/.env.worker/.env.gateway and server-join.env are
# on-host runtime config generated on the node (or, for server-join.env,
# copied from the infra node to each further server); they never crossed the
# air gap and hold secrets, so they are excluded from transport-integrity.
# *.example templates ARE covered. Single-sourced here so manifest_generate,
# manifest_verify, and the pristine check (manifest_no_hostenv) share one
# exclusion set and can never drift.
_MANIFEST_HOSTENV=(.env.infra .env.server .env.worker .env.gateway server-join.env)

# Hash every bundle file except the manifest itself and the on-host env files,
# relative to and sorted within `dir`. Shared by generate + verify so the
# exclusion is expressed exactly once.
_manifest_hashall() {
  local dir="$1" ex=( ! -name manifest.sha256 ) n prune=()
  for n in "${_MANIFEST_HOSTENV[@]}"; do ex+=( ! -name "$n" ); done
  # Optional plugins enabled on this host (enable-plugin.sh) are copies of
  # compose/plugins-available/<name>, which stays hashed; manifest_verify
  # checks each copy against it instead of hashing it twice.
  for n in $(_manifest_enabled_optional "$dir"); do
    prune+=( -path "./compose/plugins/$n" -prune -o )
  done
  ( cd "$dir"
    find . "${prune[@]}" -type f "${ex[@]}" -print0 \
      | LC_ALL=C sort -z \
      | xargs -0 "${_MANIFEST_SHA256[@]}"
  )
}
# Names of optional plugins that have been enabled in this bundle.
_manifest_enabled_optional() {
  local dir="$1" d
  for d in "$dir"/compose/plugins-available/*/; do
    [ -d "$d" ] && [ -d "$dir/compose/plugins/$(basename "$d")" ] && basename "$d"
  done
  return 0
}

manifest_generate() {
  local dir="$1"
  _manifest_hashall "$dir" > "$dir/manifest.sha256"
}

manifest_verify() {
  local dir="$1" n
  [ -f "$dir/manifest.sha256" ] || { echo "manifest.sha256 missing" >&2; return 1; }
  for n in $(_manifest_enabled_optional "$dir"); do
    diff -rq "$dir/compose/plugins/$n" "$dir/compose/plugins-available/$n" >/dev/null 2>&1 || {
      echo "manifest verification failed: enabled plugin $n differs from compose/plugins-available/$n" >&2
      return 1
    }
  done
  local tmp; tmp="$(mktemp)"
  _manifest_hashall "$dir" > "$tmp" 2>/dev/null
  if diff -q "$tmp" "$dir/manifest.sha256" >/dev/null 2>&1; then
    rm -f "$tmp"; echo OK; return 0
  else
    rm -f "$tmp"; echo "manifest verification failed" >&2; return 1
  fi
}

# Assert `dir` carries NONE of the given host-env basenames (default: all of
# those excluded above). Those files are the manifest's integrity blind spot: excluded
# so operator-generated on-host config doesn't break verify, which also means a
# planted one rides a "verified" bundle undetected. A pristine bundle (fresh off
# transport, before any generation) must contain none of them. Prints each
# offender (path relative to the bundle root); returns 0 if clean, 1 otherwise.
manifest_no_hostenv() {
  local dir="$1"; shift
  local names=( "$@" ); [ "${#names[@]}" -gt 0 ] || names=( "${_MANIFEST_HOSTENV[@]}" )
  local found=0 name hit
  for name in "${names[@]}"; do
    while IFS= read -r -d '' hit; do
      echo "pristine check: unexpected on-host env file present: ${hit#./}" >&2
      found=1
    done < <( cd "$dir" && find . -type f -name "$name" -print0 )
  done
  [ "$found" = 0 ]
}
