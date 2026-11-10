#!/usr/bin/env bash
# Thin convenience wrapper for bringing up a worker box on the offline contest
# LAN. It only sets the documented offline/LAN/worker-id environment and then
# hands off to the bundle's ../install.sh worker -- it does NOT reimplement any
# install.sh logic. For full control just call ../install.sh worker yourself
# (see ./README.md).
#
# Expects connection.env (copied from the infra box) in the bundle root.
#
#   ./bring-up-worker.sh [--id worker-2] [--image <icpc-image-tag>]
#
# Without --id, install.sh gives the worker an id unique to this machine
# (derived from its machine-id), so live-boot boxes that all share one
# hostname never collide.
#
# If --image / BROCCOLI_WORKER_IMAGE is set, the install runs non-interactively;
# otherwise install.sh prompts you to pick the worker image (choose icpc).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUNDLE="$(cd "$HERE/.." && pwd)"

WID=""
while [ $# -gt 0 ]; do
  case "$1" in
    --id) WID="$2"; shift 2 ;;
    --image) export BROCCOLI_WORKER_IMAGE="$2"; shift 2 ;;
    -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

[ -x "$BUNDLE/install.sh" ] || { echo "install.sh not found at $BUNDLE (run from the extracted bundle)" >&2; exit 2; }
[ -f "$BUNDLE/connection.env" ] || { echo "connection.env not found in $BUNDLE -- copy it from the infra box first" >&2; exit 2; }

[ -z "$WID" ] || export BROCCOLI__WORKER__ID="$WID"
# Only go non-interactive if the image is pinned; otherwise let install.sh ask.
[ -n "${BROCCOLI_WORKER_IMAGE:-}" ] && export BROCCOLI_NONINTERACTIVE=1

echo ">> install.sh worker  (id=${WID:-<unique to this machine>}, image=${BROCCOLI_WORKER_IMAGE:-<prompt>})"
cd "$BUNDLE"
exec ./install.sh worker
