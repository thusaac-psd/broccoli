#!/usr/bin/env bash
# A re-run of install-native.sh restarts the units it rewrote: `enable --now`
# leaves a running unit alone, so an upgraded weed kept its LAN-wide bind (and
# :8333, so the S3 socket failed and the install stopped before the server
# unit and connection.env), and weed, server and worker kept old credentials.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
inst="$here/../install-native.sh"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
fail() { echo "FAIL: $*"; exit 1; }

# Run the unit bring-up with systemctl recorded instead of run.
# shellcheck source=/dev/null
. <(sed -n '/^start_infra_units(){/,/^}/p; /^start_worker_units(){/,/^}/p' "$inst")
systemctl() { printf '%s\n' "$*" >> "$T/calls"; }
sleep() { :; }
start_infra_units
start_worker_units
calls="$(cat "$T/calls")"

grep -q -- '--now' "$T/calls" && fail "a unit is still started with enable --now, which never restarts it"
line() { grep -nxF "$1" "$T/calls" | head -n 1 | cut -d: -f1; }
stop="$(grep -n '^stop .*broccoli-weed' "$T/calls" | head -n 1 | cut -d: -f1)"
sock="$(line 'restart broccoli-weed-s3.socket')"
[ -n "$stop" ] && [ -n "$sock" ] && [ "$stop" -lt "$sock" ] \
  || fail "the old weed is not stopped before the S3 socket takes :8333: $calls"
for unit in broccoli-weed broccoli-server broccoli-worker broccoli-isolate-setup; do
  [ -n "$(line "restart $unit")" ] || fail "$unit is not restarted on a re-run"
done
[ "$(line 'restart broccoli-server')" -gt "$sock" ] || fail "the server restarts before object storage is back"
[ "$(line daemon-reload)" = 1 ] || fail "units are restarted before systemd reloads them"

# Both roles bring their units up this way.
grep -q '^  start_infra_units$' "$inst" || fail "install_infra_server does not use start_infra_units"
grep -q '^  start_worker_units$' "$inst" || fail "install_worker does not use start_worker_units"
echo "PASS: native re-runs restart weed, server and worker after rewriting them"
