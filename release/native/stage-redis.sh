#!/usr/bin/env bash
# Stage Redis 7 as offline .deb sets for air-gapped native installs. The
# Ubuntu archive ships Redis 7 only from 24.04 on, so the set comes from
# packages.redis.io. See stage-debs.sh.
#   ./stage-redis.sh                 # stages 20.04 22.04 24.04 (default)
#   ./stage-redis.sh 22.04           # just one version
# Output: release/native/redis/<ver>/*.deb, folded into the bundle by
# build-native-bundle.sh.
set -euo pipefail
STAGE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=stage-debs.sh
. "$STAGE_DIR/stage-debs.sh"
# shellcheck disable=SC2016 # expanded in the staging container
stage_debs redis '
  apt-get install -y -qq --no-install-recommends ca-certificates curl gnupg
  curl -fsSL https://packages.redis.io/gpg | gpg --dearmor -o /usr/share/keyrings/redis-archive-keyring.gpg
  . /etc/os-release
  echo "deb [signed-by=/usr/share/keyrings/redis-archive-keyring.gpg] https://packages.redis.io/deb ${VERSION_CODENAME} main" \
    > /etc/apt/sources.list.d/redis.list
  apt-get update -qq
' "redis-server redis-tools" "$@"
