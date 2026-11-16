#!/usr/bin/env bash
# Installers never derive a node id: they ask the role's image (node_id_of),
# hand it this machine's machine-id, record the answer in the env file so it
# survives container recreation, keep an id already set, and fail loudly when
# the image cannot answer. The derivation itself is the binary's, pinned by
# common::node_identity's tests.
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
export PATH="$here/bin:$PATH"

printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n' > "$T/machine-a"
printf 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n' > "$T/machine-b"
ask() { BROCCOLI_MACHINE_ID_FILE="$T/$1" node_id_of docker img "$2"; }
a="$(ask machine-a worker)" || fail "node_id_of failed"
case "$a" in worker-fake-*) ;; *) fail "node_id_of did not return the image's answer: $a" ;; esac
[ "$a" != "$(ask machine-b worker)" ] || fail "the host machine-id did not reach the image"
[ "$(ask machine-a server)" != "$a" ] || fail "server and worker asked the same binary"
! FAKE_ENGINE_FAIL=1 ask machine-a worker >/dev/null || fail "a failing image produced an id"
! node_id_of docker img gateway >/dev/null 2>&1 || fail "a role without a binary produced an id"

# An answer that is not a valid id is refused, not recorded.
mkdir -p "$T/bad"
printf '#!/bin/sh\necho "not an id!"\n' > "$T/bad/docker"; chmod +x "$T/bad/docker"
! PATH="$T/bad:$PATH" node_id_of docker img worker >/dev/null || fail "an invalid answer was accepted"

cp -r "$rel/." "$T/bundle"
cd "$T/bundle"
run() { BROCCOLI_MACHINE_ID_FILE="$T/machine-a" BROCCOLI_DRY_RUN=1 \
  BROCCOLI_INFRA_HOST=10.0.0.10 ./install.sh "$@" </dev/null >/dev/null 2>&1; }
run infra || fail "infra dry run"
run worker || fail "worker dry run"
run server || fail "server dry run"
[ "$(env_get .env.worker BROCCOLI__WORKER__ID)" = "$a" ] || fail "install.sh worker did not record the image's id"
[ "$(env_get .env.server BROCCOLI__SERVER__ID)" = "$(ask machine-a server)" ] \
  || fail "install.sh server did not record the image's id"
rm .env.worker
BROCCOLI__WORKER__ID=worker-7 run worker || fail "worker dry run with an id"
[ "$(env_get .env.worker BROCCOLI__WORKER__ID)" = worker-7 ] || fail "install.sh replaced a given id"
rm .env.worker
! FAKE_ENGINE_FAIL=1 run worker || fail "install.sh went on without an id"
echo "PASS: installers record the id the image derives, keep a given one, and refuse without one"
