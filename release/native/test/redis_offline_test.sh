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
# ensure_redis7 tries the bundle first, and only then any network source.
redis_fn="$(awk '/^ensure_redis7\(\)\{/,/^\}/' "$inst")"
[ -n "$redis_fn" ] || fail "ensure_redis7 not found"
bundle_at=$(printf '%s\n' "$redis_fn" | grep -n 'BUNDLE/redis' | head -1 | cut -d: -f1)
apt_at=$(printf '%s\n' "$redis_fn" | grep -nE 'apt_ensure|packages\.redis\.io' | head -1 | cut -d: -f1)
[ -n "$bundle_at" ] || fail "ensure_redis7 does not install from the bundle"
[ -n "$apt_at" ] || fail "ensure_redis7 lost its online fallback"
[ "$bundle_at" -lt "$apt_at" ] || fail "ensure_redis7 reaches the network before the bundle"

# dpkg from the bundle runs offline (no network flags).
printf '%s\n' "$redis_fn" | grep -q 'dpkg -i' || fail "ensure_redis7 does not dpkg the bundled set"

echo "PASS: native Redis 7 installs from the bundle offline, network only as a fallback"
