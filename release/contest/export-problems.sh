#!/usr/bin/env bash
# Export problems, or a whole contest, from this node's server into one
# portable .tar.gz that import-problems.sh restores onto another server. Runs
# `broccoli-server bundle export` with the server's own configuration, so it
# reads every storage backend and the live schema; nothing is fetched from the
# network.
#
# Usage, on the server node:
#   ./export-problems.sh [--contest N | --problems 1,2,3] [--with-secrets]
#                        [--out broccoli-problems-<UTC>.tar.gz]
#   --contest N     the contest's problems, the contest, its roster and the
#                   roster's accounts and roles
#   --problems IDS  only these problems (default: every problem not deleted)
#   --with-secrets  keep password hashes (contest bundles; default: blanked)
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/server-cli.sh
. "$HERE/../lib/server-cli.sh"

OUT="" args=()
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --contest | --problems) args+=("$1" "$2"); shift 2 ;;
    --with-secrets) args+=("$1"); shift ;;
    -h | --help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
OUT="${OUT:-broccoli-problems-$(date -u +%Y%m%d-%H%M%S).tar.gz}"

if ! server_cli bundle export "${args[@]}" > "$OUT"; then
  rm -f "$OUT"
  exit 1
fi
echo ">> wrote $OUT ($(du -h "$OUT" | cut -f1))"
