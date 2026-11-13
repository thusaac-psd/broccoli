#!/usr/bin/env bash
# An air-gapped native infra box gets Redis 7 from the bundle, not the network.
# stage-redis.sh captures the .deb set, build-native-bundle folds it in, and
# ensure_redis7 installs from the bundle before any apt or packages.redis.io.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
native="$here/.."
fail() { echo "FAIL: $*"; exit 1; }

[ -x "$native/stage-redis.sh" ] || fail "stage-redis.sh is missing or not executable"

# The staging script downloads amd64 Redis 7 .debs into redis/<ver>/.
grep -q 'redis/\$ver' "$native/stage-redis.sh" || fail "stage-redis.sh does not write redis/<ver>"
grep -q -- '--download-only' "$native/stage-redis.sh" || fail "stage-redis.sh installs instead of downloading"
grep -q 'platform linux/amd64' "$native/stage-redis.sh" || fail "stage-redis.sh does not pin amd64"

# The bundle builder ships redis/ when it was staged.
grep -q 'cp -a "$HERE/redis" "$WORK/redis"' "$native/build-native-bundle.sh" \
  || fail "build-native-bundle does not bundle redis/"

inst="$native/install-native.sh"
# Run the installer's own ensure_redis7 against stubbed dpkg, apt and Redis.
fns="$(awk '/^redis_major\(\)\{/,/\}$/; /^ensure_redis7\(\)\{/,/^\}/' "$inst")"
printf '%s\n' "$fns" | grep -q '^ensure_redis7(){' || fail "ensure_redis7 not found"

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
ver="$(. /etc/os-release 2>/dev/null; echo "${VERSION_ID:-}")"

# scenario NAME: a fresh state; the stubs read and write files under $T/NAME.
# Knobs: installed (Redis major now), bundle (1 = a bundled set), dpkg_fails,
# dpkg_status, bundle_gives, apt_gives, curl_fails.
run() {
  local name="$1"
  shift
  local dir="$T/$name" status=0
  mkdir -p "$dir/bundle"
  (
    for kv in "$@"; do printf '%s\n' "$kv"; done
  ) > "$dir/knobs"
  # shellcheck disable=SC2016
  (
    set +e
    knob() { sed -n "s/^$1=//p" "$dir/knobs"; }
    BUNDLE="$dir/bundle"
    if [ "$(knob bundle)" = 1 ]; then
      mkdir -p "$BUNDLE/redis/$ver"
      touch "$BUNDLE/redis/$ver/redis-server_7.4.1_amd64.deb" "$BUNDLE/redis/$ver/redis-tools_7.4.1_amd64.deb"
    fi
    knob installed > "$dir/version"
    calls() { echo "$*" >> "$dir/calls"; }
    redis-server() { local v; v="$(cat "$dir/version")"; [ -n "$v" ] && echo "Redis server v=$v.0.0 sha=0"; }
    dpkg() {
      calls "dpkg $*"
      if [ "$(knob dpkg_fails)" = 1 ]; then
        echo "dpkg: dependency problems prevent configuration of redis-server" >&2
        return 1
      fi
      knob bundle_gives > "$dir/version"
    }
    dpkg-query() { calls "dpkg-query $*"; printf '%s' "$(knob dpkg_status)"; }
    apt_ensure() { calls "apt_ensure $*"; knob apt_gives > "$dir/version"; }
    apt-get() { calls "apt-get $*"; [ "$1" = update ] && return 0; echo 7 > "$dir/version"; }
    curl() { calls "curl $*"; [ "$(knob curl_fails)" != 1 ]; }
    gpg() { cat > /dev/null; }
    die() { echo "DIE: $*"; exit 3; }
    eval "$fns"
    ensure_redis7
  ) > "$dir/out" 2>&1 || status=$?
  echo "$status" > "$dir/rc"
  touch "$dir/calls"
}
rc() { cat "$T/$1/rc"; }
called() { grep -q "$2" "$T/$1/calls"; }

run current installed=7
[ "$(rc current)" = 0 ] || fail "an installed Redis 7 was not accepted: $(cat "$T/current/out")"
[ ! -s "$T/current/calls" ] || fail "an installed Redis 7 was reinstalled: $(cat "$T/current/calls")"

run offline installed=6 bundle=1 bundle_gives=7 dpkg_status='install ok installed' apt_gives=6 curl_fails=1
[ "$(rc offline)" = 0 ] || fail "the bundled set did not install: $(cat "$T/offline/out")"
called offline '^dpkg -i --skip-same-version' || fail "the bundled set was not given to dpkg"
! called offline 'apt_ensure\|apt-get\|curl' || fail "an offline install reached for the network: $(cat "$T/offline/calls")"

run broken installed=6 bundle=1 dpkg_fails=1 apt_gives=7
[ "$(rc broken)" = 3 ] || fail "a failing dpkg did not stop the install"
grep -q 'dependency problems' "$T/broken/out" || fail "dpkg's own error was not shown: $(cat "$T/broken/out")"
! called broken 'apt_ensure\|apt-get' || fail "a failing bundled set fell through to apt"

run half installed=6 bundle=1 bundle_gives=7 dpkg_status='install ok half-configured'
[ "$(rc half)" = 3 ] || fail "a half-configured redis-server was accepted"
grep -q "half-configured" "$T/half/out" || fail "the dpkg status was not reported: $(cat "$T/half/out")"

run stale installed=6 bundle=1 bundle_gives=6 dpkg_status='install ok installed'
[ "$(rc stale)" = 3 ] || fail "a bundled Redis 6 was accepted"

run online installed=6 apt_gives=7
[ "$(rc online)" = 0 ] || fail "the system apt's Redis 7 was not accepted: $(cat "$T/online/out")"
called online '^apt_ensure redis-server' || fail "without a bundle the system apt was not tried"
! called online 'curl' || fail "packages.redis.io was tried although apt had Redis 7"

run repo installed=6 apt_gives=6
[ "$(rc repo)" = 0 ] || fail "packages.redis.io's Redis 7 was not accepted: $(cat "$T/repo/out")"
called repo 'apt-get install -y -qq redis' || fail "an old system Redis did not reach packages.redis.io"

run airgap installed=6 apt_gives=6 curl_fails=1
[ "$(rc airgap)" = 3 ] || fail "an air-gapped box without a bundle and with Redis 6 did not stop"
grep -q 'needs Redis 7' "$T/airgap/out" || fail "the air-gap failure does not say what is needed"

echo "PASS: native Redis 7 installs from the bundle offline, network only as a fallback"
