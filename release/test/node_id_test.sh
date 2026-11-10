#!/usr/bin/env bash
# Default server and worker ids are stable on one machine, differ between
# machines that share a hostname, and are valid ids; the installer writes them.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
rel="$(cd "$here/.." && pwd)"
# shellcheck source=../lib/host.sh
. "$rel/lib/host.sh"
# shellcheck source=../lib/env.sh
. "$rel/lib/env.sh"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
fail() { echo "FAIL: $*"; exit 1; }

printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n' > "$T/machine-a"
printf 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n' > "$T/machine-b"
a1="$(BROCCOLI_MACHINE_ID_FILE="$T/machine-a" node_id worker)"
a2="$(BROCCOLI_MACHINE_ID_FILE="$T/machine-a" node_id worker)"
b="$(BROCCOLI_MACHINE_ID_FILE="$T/machine-b" node_id worker)"
[ "$a1" = "$a2" ] || fail "id is not stable on one machine ($a1 vs $a2)"
[ "$a1" != "$b" ] || fail "two machines with one hostname got the same id ($a1)"
case "$a1" in
  worker-*) ;;
  *) fail "id lacks its role prefix: $a1" ;;
esac
# The server's own validation: [A-Za-z0-9._-], at most 128 characters.
case "$a1" in *[!A-Za-z0-9._-]*) fail "id has characters a server id may not: $a1" ;; esac
[ "${#a1}" -le 128 ] || fail "id longer than 128 characters"
[ "$(BROCCOLI_MACHINE_ID_FILE="$T/machine-a" node_id server)" != "$a1" ] || fail "server and worker ids collide"

cp -r "$rel/." "$T/bundle"
cd "$T/bundle"
run() { BROCCOLI_MACHINE_ID_FILE="$T/machine-a" BROCCOLI_DRY_RUN=1 BROCCOLI_SKIP_TIME_CHECK=1 \
  BROCCOLI_INFRA_HOST=10.0.0.10 ./install.sh "$@" </dev/null >/dev/null 2>&1; }
run infra || fail "infra dry run"
run worker || fail "worker dry run"
run server || fail "server dry run"
[ "$(env_get .env.worker BROCCOLI__WORKER__ID)" = "$a1" ] || fail "install.sh worker did not default to the machine's id"
[ "$(env_get .env.server BROCCOLI__SERVER__ID)" = "$(BROCCOLI_MACHINE_ID_FILE="$T/machine-a" node_id server)" ] \
  || fail "install.sh server did not default to the machine's id"
echo "PASS: default node ids are stable, unique per machine, and valid"
