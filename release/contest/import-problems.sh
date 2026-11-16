#!/usr/bin/env bash
# Restore an archive made by export-problems.sh onto this node's server. Runs
# `broccoli-server bundle import` with the server's own configuration, so the
# rows go into its database and the blobs into its storage backend. The server
# must have started once with this build, so the database is migrated.
#
# Usage, on the server node:
#   ./import-problems.sh ARCHIVE.tar.gz [--replace-problems [--wipe-dependents]]
#   --replace-problems  replace the problems the server already has
#   --wipe-dependents   also delete the submissions, code runs and contest links
#                       that reference them
# Accounts merge by username: an active account the server has keeps its
# password and id, and the bundle's roster and roles follow it.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/server-cli.sh
. "$HERE/../lib/server-cli.sh"

ARCHIVE="" args=()
while [ $# -gt 0 ]; do
  case "$1" in
    --replace-problems | --wipe-dependents) args+=("$1"); shift ;;
    -h | --help) sed -n '2,13p' "$0"; exit 0 ;;
    -*) echo "unknown arg: $1" >&2; exit 2 ;;
    *) [ -z "$ARCHIVE" ] || { echo "unexpected arg: $1" >&2; exit 2; }; ARCHIVE="$1"; shift ;;
  esac
done
[ -n "$ARCHIVE" ] || { sed -n '7,9p' "$0" >&2; exit 2; }
[ -f "$ARCHIVE" ] || { echo "archive not found: $ARCHIVE" >&2; exit 2; }

server_cli bundle import "${args[@]}" < "$ARCHIVE"
