#!/usr/bin/env bash
# Installers never derive a node id: they ask the role's loaded image
# (node_id_of, which never lets the engine pull), hand it this machine's
# machine-id, keep an id already set, and fail loudly when the image cannot
# answer. A dry run needs no engine: it leaves the id blank for the binary to
# derive at boot. The derivation itself is the binary's, pinned by
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
# The stub engine answers only a question that passes --pull never.
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

# A dry run runs without any container engine: a docker on PATH that records
# being called stays uncalled, and the id is left blank, with the comment
# saying the binary derives it.
mkdir -p "$T/nodocker"
printf '#!/bin/sh\ntouch "%s/docker-called"\nexit 1\n' "$T" > "$T/nodocker/docker"
chmod +x "$T/nodocker/docker"
cp -r "$rel/." "$T/bundle"
cd "$T/bundle"
run() { PATH="$T/nodocker:$PATH" BROCCOLI_MACHINE_ID_FILE="$T/machine-a" BROCCOLI_DRY_RUN=1 \
  BROCCOLI_INFRA_HOST=10.0.0.10 ./install.sh "$@" </dev/null >/dev/null 2>&1; }
run infra || fail "infra dry run"
run worker || fail "worker dry run"
run server || fail "server dry run"
[ ! -e "$T/docker-called" ] || fail "a dry run ran the container engine"
for role in worker server; do
  key="BROCCOLI__$(printf '%s' "$role" | tr '[:lower:]' '[:upper:]')__ID"
  grep -qx "$key=''" ".env.$role" || fail "a $role dry run recorded an id: $(grep "^$key=" ".env.$role")"
  grep -q "^# $key: .*Blank means the binary derives it at boot" ".env.$role" \
    || fail "the $role env file does not say a blank id is derived at boot"
done
run single-host || fail "single-host dry run"
for key in BROCCOLI__SERVER__ID BROCCOLI__WORKER__ID; do
  grep -qx "$key=''" .env.single-host || fail "a single-host dry run recorded $key: $(grep "^$key=" .env.single-host)"
done
rm .env.worker
BROCCOLI__WORKER__ID=worker-7 run worker || fail "worker dry run with an id"
[ "$(env_get .env.worker BROCCOLI__WORKER__ID)" = worker-7 ] || fail "install.sh replaced a given id"
# The shipped env examples name no node id, so a copied example never gives
# two nodes the same one.
for f in "$rel"/.env*.example; do
  ! grep -Eq '^BROCCOLI__(SERVER|WORKER)__ID=.' "$f" || fail "$f fixes a node id"
done
echo "PASS: installers ask the loaded image for the id without pulling, keep a given one, and leave it to the binary in a dry run"
