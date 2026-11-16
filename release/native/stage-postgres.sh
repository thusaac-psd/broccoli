#!/usr/bin/env bash
# Stage PostgreSQL as offline .deb sets for air-gapped native installs, the
# same major version the release's compose files run (POSTGRES_IMAGE in
# release/.env.infra.example). The Ubuntu archive ships older majors, so the
# set comes from the PostgreSQL project's APT repository (apt.postgresql.org).
# See stage-debs.sh.
#   ./stage-postgres.sh              # stages 20.04 22.04 24.04 (default)
#   ./stage-postgres.sh 22.04        # just one version
# Output: release/native/postgres/<ver>/*.deb, folded into the bundle by
# build-native-bundle.sh.
set -euo pipefail
STAGE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=stage-debs.sh
. "$STAGE_DIR/stage-debs.sh"
# PG_MAJOR overrides the major, for a copy of this script outside the repo.
image="${PG_MAJOR:-$(sed -n 's/^POSTGRES_IMAGE=postgres:\([0-9]*\).*/\1/p' "$STAGE_DIR/../.env.infra.example" 2>/dev/null)}"
[ -n "$image" ] || { echo "error: set PG_MAJOR, or run from release/native next to release/.env.infra.example" >&2; exit 1; }
# shellcheck disable=SC2016 # expanded in the staging container
stage_debs postgres '
  apt-get install -y -qq --no-install-recommends ca-certificates curl gnupg
  curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc | gpg --dearmor -o /usr/share/keyrings/pgdg.gpg
  . /etc/os-release
  echo "deb [signed-by=/usr/share/keyrings/pgdg.gpg] https://apt.postgresql.org/pub/repos/apt ${VERSION_CODENAME}-pgdg main" \
    > /etc/apt/sources.list.d/pgdg.list
  apt-get update -qq
' "postgresql-$image postgresql-client-$image" "$@"
