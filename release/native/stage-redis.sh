#!/usr/bin/env bash
# Stage Redis 7 as offline .deb sets for air-gapped native installs. An
# infra-server box in a closed lab cannot reach packages.redis.io, and the
# Ubuntu archive ships Redis 7 only from 24.04 on, so install-native.sh
# (infra role) installs the set matching the box's Ubuntu VERSION_ID from
# redis/<ver>/*.deb before it falls back to apt.
#
# Run on ANY machine with Docker + internet (it does NOT need to match the
# target Ubuntu version; each set is built inside that version's container):
#   ./stage-redis.sh                 # stages 20.04 22.04 24.04 (default)
#   ./stage-redis.sh 22.04           # just one version
# Output: release/native/redis/<ver>/*.deb, folded into the contest bundle by
# build-native-bundle.sh. Re-run any time to refresh.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERS=("$@"); [ ${#VERS[@]} -gt 0 ] || VERS=(20.04 22.04 24.04)

command -v docker >/dev/null 2>&1 || {
  echo "error: docker not found; run this on a box with Docker and internet." >&2
  exit 1
}

for ver in "${VERS[@]}"; do
  dest="$HERE/redis/$ver"
  mkdir -p "$dest"
  echo ">> staging Redis 7 for Ubuntu $ver -> $dest"
  rm -f "$dest"/*.deb
  # --platform pins amd64: the whole bundle is x86_64, so the .debs must be
  # amd64 even when staging from an arm64 host (qemu-emulated). Add the Redis
  # APT repo, then download redis 7 and its deps WITHOUT installing.
  docker run --rm --platform linux/amd64 -v "$dest:/out" "ubuntu:$ver" bash -c '
    set -e
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends ca-certificates curl gnupg
    curl -fsSL https://packages.redis.io/gpg | gpg --dearmor -o /usr/share/keyrings/redis-archive-keyring.gpg
    . /etc/os-release
    echo "deb [signed-by=/usr/share/keyrings/redis-archive-keyring.gpg] https://packages.redis.io/deb ${VERSION_CODENAME} main" \
      > /etc/apt/sources.list.d/redis.list
    apt-get update -qq
    apt-get install -y --download-only --no-install-recommends redis-server redis-tools
    cp /var/cache/apt/archives/*.deb /out/ 2>/dev/null || true
    chmod 644 /out/*.deb 2>/dev/null || true
  ' || { echo "  WARN: staging $ver failed (image pull or apt). skipping." >&2; continue; }
  n=$(ls "$dest"/*.deb 2>/dev/null | wc -l | tr -d ' ')
  if [ "$n" = 0 ]; then
    echo "  WARN: no .debs captured for $ver" >&2
  elif ! ls "$dest"/redis-server_7* >/dev/null 2>&1 && ! ls "$dest"/redis-server_8* >/dev/null 2>&1; then
    echo "  WARN: $ver captured $n debs but no redis-server 7+; check the repo" >&2
  else
    echo "  staged $ver: $n debs ($(du -sh "$dest" | cut -f1))"
  fi
done

echo ">> done. redis sets under $HERE/redis/ ; rebuild the bundle to include them."
