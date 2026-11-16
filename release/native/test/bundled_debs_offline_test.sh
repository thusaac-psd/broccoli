#!/usr/bin/env bash
# An air-gapped native infra box gets PostgreSQL and Redis 7 from the bundle,
# never the network. stage-redis.sh and stage-postgres.sh capture the .deb
# sets, build-native-bundle folds them in, and the installer installs from the
# bundle, keeps what the box already has, and otherwise stops with the
# staging command.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
native="$here/.."
fail() { echo "FAIL: $*"; exit 1; }

for s in stage-redis.sh stage-postgres.sh stage-toolchain.sh; do
  [ -x "$native/$s" ] || fail "$s is missing or not executable"
  grep -q '^stage_debs ' "$native/$s" || fail "$s does not stage through stage_debs"
done

# stage_debs, run with a stub docker, hands the container the set's packages
# and writes into <name>/<ver>/.
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/bin"
cat > "$T/bin/docker" <<'E'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$STUB_ARGS"
for a in "$@"; do case "$a" in *:/out) touch "${a%:/out}/stub_1_amd64.deb" ;; esac; done
E
chmod +x "$T/bin/docker"
( export PATH="$T/bin:$PATH" STUB_ARGS="$T/args" STAGE_DIR="$T/stage"
  . "$native/stage-debs.sh"
  stage_debs postgres 'echo setup' 'postgresql-18 postgresql-client-18' 22.04 ) > "$T/stage.out"
[ -f "$T/stage/postgres/22.04/stub_1_amd64.deb" ] || fail "stage_debs did not write postgres/22.04"
grep -qx 'linux/amd64' "$T/args" || fail "stage_debs does not pin amd64"
grep -qx 'PACKAGES=postgresql-18 postgresql-client-18' "$T/args" || fail "stage_debs did not pass the packages"
grep -q -- '--download-only' "$T/args" || fail "stage_debs installs instead of downloading"
grep -q 'Dir::State::status=/tmp/base-status' "$T/args" \
  || fail "stage_debs resolves against what its setup installed, not the base image"
# The PostgreSQL major is the one the release compose files run.
major="$(sed -n 's/^POSTGRES_IMAGE=postgres:\([0-9]*\).*/\1/p' "$native/../.env.infra.example")"
mkdir -p "$T/release/native"
cp "$native/stage-postgres.sh" "$native/stage-debs.sh" "$T/release/native/"
cp "$native/../.env.infra.example" "$T/release/"
( export PATH="$T/bin:$PATH" STUB_ARGS="$T/pgargs"; bash "$T/release/native/stage-postgres.sh" 24.04 ) >/dev/null
grep -qx "PACKAGES=postgresql-$major postgresql-client-$major" "$T/pgargs" \
  || fail "stage-postgres.sh does not stage PostgreSQL $major: $(grep PACKAGES "$T/pgargs")"

# The bundle builder ships postgres/ and redis/ when they were staged.
grep -q 'for set in postgres redis; do' "$native/build-native-bundle.sh" \
  || fail "build-native-bundle does not bundle postgres/ and redis/"

inst="$native/install-native.sh"
# Run the installer's own functions against stubbed dpkg, apt, Redis and PostgreSQL.
fns="$(awk '/^ubuntu_version\(\)\{/,/\}$/; /^first_match\(\)\{/,/^\}/; /^die_unstaged\(\)\{/,/^\}/; /^install_bundled_set\(\)\{/,/^\}/; /^redis_major\(\)\{/,/\}$/; /^ensure_redis7\(\)\{/,/^\}/; /^ensure_postgres\(\)\{/,/^\}/; /^ensure_toolchain\(\)\{/,/^\}/' "$inst")"
for f in ubuntu_version first_match die_unstaged install_bundled_set ensure_redis7 ensure_postgres ensure_toolchain; do
  printf '%s\n' "$fns" | grep -q "^$f(){" || fail "$f not found"
done

ver="$(. /etc/os-release 2>/dev/null; echo "${VERSION_ID:-}")"

# scenario NAME: a fresh state; the stubs read and write files under $T/NAME.
# Knobs: installed (Redis major now), bundle (1 = a bundled set), dpkg_fails,
# dpkg_status, bundle_gives.
run() {
  local name="$1"
  shift
  local dir="$T/$name" status=0
  mkdir -p "$dir/bundle"
  (
    for kv in "$@"; do printf '%s\n' "$kv"; done
  ) > "$dir/knobs"
  # The installer's own shell options: a step that fails silently under
  # them stops the install without saying why. The scenario runs as a job of
  # its own, since `set -e` is off in a subshell an `||` tests.
  # shellcheck disable=SC2016
  (
    set -euo pipefail
    knob() { sed -n "s/^$1=//p" "$dir/knobs"; }
    BUNDLE="$dir/bundle"
    if [ "$(knob bundle)" = 1 ]; then
      mkdir -p "$BUNDLE/redis/$ver"
      touch "$BUNDLE/redis/$ver/redis-server_7.4.1_amd64.deb" "$BUNDLE/redis/$ver/redis-tools_7.4.1_amd64.deb"
    fi
    if [ "$(knob pg_bundle)" = 1 ]; then
      mkdir -p "$BUNDLE/postgres/$ver"
      touch "$BUNDLE/postgres/$ver/postgresql-18_18.7-1_amd64.deb" "$BUNDLE/postgres/$ver/libpq5_18.7-1_amd64.deb"
    fi
    knob installed > "$dir/version"
    calls() { echo "$*" >> "$dir/calls"; }
    redis-server() { local v; v="$(cat "$dir/version")"; [ -n "$v" ] && echo "Redis server v=$v.0.0 sha=0"; }
    dpkg() {
      calls "dpkg $*"
      if [ "$1" = -s ]; then [ "$(knob toolchain_installed)" = 1 ]; return; fi
      if [ "$(knob dpkg_fails)" = 1 ]; then
        echo "dpkg: dependency problems prevent configuration of redis-server" >&2
        return 1
      fi
      knob bundle_gives > "$dir/version"
    }
    dpkg-query() {
      calls "dpkg-query $*"
      case "$*" in
        *"postgresql-[0-9]*"*) [ -n "$(knob pg_installed)" ] && echo "$(knob pg_installed) install ok installed" ;;
        *) printf '%s' "$(knob dpkg_status)" ;;
      esac
    }
    apt-get() { calls "apt-get $*"; [ "$1" = update ] && return 0; echo 7 > "$dir/version"; }
    curl() { calls "curl $*"; }
    gpg() { cat > /dev/null; }
    die() { echo "DIE: $*"; exit 3; }
    eval "$fns"
    "${ENSURE:-ensure_redis7}"
  ) > "$dir/out" 2>&1 &
  wait $! || status=$?
  echo "$status" > "$dir/rc"
  touch "$dir/calls"
}
rc() { cat "$T/$1/rc"; }
called() { grep -q "$2" "$T/$1/calls"; }

run current installed=7
[ "$(rc current)" = 0 ] || fail "an installed Redis 7 was not accepted: $(cat "$T/current/out")"
[ ! -s "$T/current/calls" ] || fail "an installed Redis 7 was reinstalled: $(cat "$T/current/calls")"

run offline installed=6 bundle=1 bundle_gives=7 dpkg_status='install ok installed' 
[ "$(rc offline)" = 0 ] || fail "the bundled set did not install: $(cat "$T/offline/out")"
called offline '^dpkg -i --skip-same-version' || fail "the bundled set was not given to dpkg"
! called offline 'apt-get\|curl' || fail "an offline install reached for the network: $(cat "$T/offline/calls")"

run broken installed=6 bundle=1 dpkg_fails=1
[ "$(rc broken)" = 3 ] || fail "a failing dpkg did not stop the install"
grep -q 'dependency problems' "$T/broken/out" || fail "dpkg's own error was not shown: $(cat "$T/broken/out")"
! called broken 'apt-get' || fail "a failing bundled set fell through to apt"

run half installed=6 bundle=1 bundle_gives=7 dpkg_status='install ok half-configured'
[ "$(rc half)" = 3 ] || fail "a half-configured redis-server was accepted"
grep -q "half-configured" "$T/half/out" || fail "the dpkg status was not reported: $(cat "$T/half/out")"

run stale installed=6 bundle=1 bundle_gives=6 dpkg_status='install ok installed'
[ "$(rc stale)" = 3 ] || fail "a bundled Redis 6 was accepted"

# Without a bundled set, an old Redis stops the install with the staging
# command; nothing reaches for apt or packages.redis.io.
run airgap installed=6
[ "$(rc airgap)" = 3 ] || fail "a box without a bundle and with Redis 6 did not stop with a reason: rc $(rc airgap), $(cat "$T/airgap/out")"
grep -q 'stage-redis.sh' "$T/airgap/out" || fail "the failure does not name the staging command: $(cat "$T/airgap/out")"
! called airgap 'apt-get\|curl' || fail "a box without a bundle reached for the network: $(cat "$T/airgap/calls")"

ENSURE=ensure_postgres run pg_offline pg_bundle=1 dpkg_status='install ok installed'
[ "$(rc pg_offline)" = 0 ] || fail "the bundled PostgreSQL set did not install: $(cat "$T/pg_offline/out")"
called pg_offline '^dpkg -i --skip-same-version' || fail "the bundled PostgreSQL set was not given to dpkg"
called pg_offline 'dpkg-query -W -f=${Status} postgresql-18' || fail "the PostgreSQL install was not verified"
! called pg_offline 'apt-get' || fail "an offline PostgreSQL install reached for apt"

ENSURE=ensure_postgres run pg_broken pg_bundle=1 dpkg_fails=1
[ "$(rc pg_broken)" = 3 ] || fail "a failing bundled PostgreSQL set did not stop the install"

ENSURE=ensure_postgres run pg_present pg_installed=postgresql-14 pg_bundle=1
[ "$(rc pg_present)" = 0 ] || fail "an installed PostgreSQL server was not kept"
! called pg_present '^dpkg -i\|apt-get' || fail "an installed PostgreSQL server was reinstalled"

ENSURE=ensure_postgres run pg_unstaged
[ "$(rc pg_unstaged)" = 3 ] || fail "a box without PostgreSQL or a bundled set did not stop with a reason: rc $(rc pg_unstaged), $(cat "$T/pg_unstaged/out")"
grep -q 'stage-postgres.sh' "$T/pg_unstaged/out" || fail "the failure does not name the staging command"
! called pg_unstaged 'apt-get\|curl' || fail "a box without a bundled PostgreSQL set reached for the network"

# Every role without its bundled set stops with the staging command, under
# the installer's own `set -euo pipefail`.
ENSURE=ensure_toolchain run tc_unstaged
[ "$(rc tc_unstaged)" = 3 ] || fail "a box without a toolchain or a bundled set did not stop with a reason: rc $(rc tc_unstaged), $(cat "$T/tc_unstaged/out")"
grep -q 'stage-toolchain.sh' "$T/tc_unstaged/out" || fail "the toolchain failure does not name the staging command"
! called tc_unstaged 'apt-get\|curl' || fail "a box without a bundled toolchain reached for the network"

echo "PASS: native PostgreSQL, Redis 7 and the toolchain install from the bundle or the box, never the network"
