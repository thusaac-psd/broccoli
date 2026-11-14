#!/usr/bin/env bash
# The shared env validator (release/lib/env.sh env_require_role) refuses an
# unchanged .env.*.example, an empty secret and a malformed URL, and every
# installer entry point runs it before starting anything: release/install.sh,
# the air-gapped install.sh (server and worker) and install-native.sh worker.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
rel="$here/.."
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
fail() { echo "FAIL: $*"; exit 1; }
# shellcheck source=../lib/env.sh
. "$rel/lib/env.sh"

# --- the validator itself ---
problems() { ( set -a; . "$1"; set +a; env_require_role "$2" 2>&1 ) || true; }
out="$(problems "$rel/.env.server.example" server)"
for k in BROCCOLI__DATABASE__URL BROCCOLI__MQ__URL BROCCOLI__AUTH__JWT_SECRET \
         BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT \
         BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY; do
  grep -q "$k still holds a change-me placeholder" <<<"$out" || fail "server example: $k not refused: $out"
done
out="$(problems "$rel/.env.worker.example" worker)"
grep -q "BROCCOLI__DATABASE__URL still holds a change-me" <<<"$out" || fail "worker example accepted: $out"
out="$(problems "$rel/.env.infra.example" infra)"
grep -q "POSTGRES_PASSWORD still holds a change-me" <<<"$out" || fail "infra example accepted: $out"

good="$T/good.env"
{
  env_line BROCCOLI__DATABASE__URL 'postgres://broccoli:p%40ss@db:5432/broccoli'
  env_line BROCCOLI__MQ__URL 'redis://:pw@redis:6379'
  env_line BROCCOLI__AUTH__JWT_SECRET 'a-real-secret-a-real-secret-a-real-secret'
  env_line BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD 'adminpw123'
  env_line BROCCOLI__STORAGE__BACKEND object_storage
  env_line BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT 'http://seaweedfs:8333'
  env_line BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY ak
  env_line BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY sk
} > "$good"
[ -z "$(problems "$good" server)" ] || fail "a complete server env was refused: $(problems "$good" server)"

bad="$T/bad.env"
cp "$good" "$bad"
env_set "$bad" BROCCOLI__DATABASE__URL 'db:5432/broccoli'
env_set "$bad" BROCCOLI__MQ__URL 'http://redis:6379'
env_set "$bad" BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT 'seaweedfs:8333'
env_set "$bad" BROCCOLI__AUTH__JWT_SECRET ''
out="$(problems "$bad" server)"
grep -q 'BROCCOLI__DATABASE__URL must look like postgres://' <<<"$out" || fail "malformed DB URL accepted: $out"
grep -q 'BROCCOLI__MQ__URL must look like redis://' <<<"$out" || fail "malformed MQ URL accepted: $out"
grep -q 'OBJECT_STORAGE__ENDPOINT must look like http://' <<<"$out" || fail "malformed S3 endpoint accepted: $out"
grep -q 'BROCCOLI__AUTH__JWT_SECRET is not set' <<<"$out" || fail "empty JWT secret accepted: $out"

# The database blob backend needs no S3 settings.
db_only="$T/db.env"
grep -v OBJECT_STORAGE "$good" > "$db_only"
env_set "$db_only" BROCCOLI__STORAGE__BACKEND database
[ -z "$(problems "$db_only" server)" ] || fail "the database backend still asked for S3 settings"

# --- the air-gapped installer refuses unchanged examples before loading images ---
mkdir -p "$T/bin"
printf '#!/usr/bin/env bash\nexit 0\n' > "$T/bin/docker"
chmod +x "$T/bin/docker"
B="$T/bundle"
mkdir -p "$B/compose"
for f in infra server worker; do cp "$rel/.env.$f.example" "$B/compose/.env.$f"; done
out="$(PATH="$T/bin:$PATH" bash "$rel/airgap/install.sh" --role server --bundle "$B" --lan-host contest.lan --dry-run 2>&1)" \
  && fail "airgap server accepted the example env files"
grep -q 'POSTGRES_PASSWORD still holds a change-me' <<<"$out" || fail "airgap server did not name the infra placeholder: $out"
grep -q 'BROCCOLI__DATABASE__URL still holds a change-me' <<<"$out" || fail "airgap server did not name the server placeholder: $out"
out="$(PATH="$T/bin:$PATH" bash "$rel/airgap/install.sh" --role worker --bundle "$B" 2>&1)" \
  && fail "airgap worker accepted the example env file"
grep -q 'BROCCOLI__MQ__URL still holds a change-me' <<<"$out" || fail "airgap worker did not name the placeholder: $out"
grep -q 'load-bundle\|images' <<<"$out" && fail "airgap worker loaded images before validating: $out"

# --- release/install.sh refuses a placeholder passed for this run ---
cp -r "$rel/." "$T/rel/"
out="$(cd "$T/rel" && BROCCOLI_DRY_RUN=1 BROCCOLI__DATABASE__URL='postgres://u:p@change-me-infra-host:5432/b' \
  BROCCOLI__MQ__URL='redis://:p@10.0.0.1:6379' BROCCOLI__AUTH__JWT_SECRET=jwt-secret-jwt-secret-jwt-secret \
  BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD=adminpw123 BROCCOLI__STORAGE__BACKEND=database \
  ./install.sh server </dev/null 2>&1)" && fail "install.sh server accepted a placeholder database URL"
grep -q 'BROCCOLI__DATABASE__URL still holds a change-me' <<<"$out" || fail "install.sh did not name the placeholder: $out"

# --- install-native.sh worker refuses an incomplete connection.env before it
#     installs anything (an `id` stub stands in for sudo) ---
mkdir -p "$T/native-bundle"
cp -r "$rel/native" "$T/native-bundle/native"
cp -r "$rel/lib" "$T/native-bundle/lib"
printf '#!/usr/bin/env bash\necho 0\n' > "$T/bin/id"
chmod +x "$T/bin/id"
printf "BROCCOLI_INFRA_HOST='10.0.0.1'\nBROCCOLI__DATABASE__URL='postgres://broccoli:pw@10.0.0.1:5432/broccoli'\n" \
  > "$T/native-bundle/native/connection.env"
out="$(PATH="$T/bin:$PATH" bash "$T/native-bundle/native/install-native.sh" worker 2>&1)" \
  && fail "install-native.sh worker accepted an incomplete connection.env"
grep -q 'BROCCOLI__MQ__URL is not set' <<<"$out" || fail "install-native.sh did not name the missing MQ URL: $out"
grep -q 'OBJECT_STORAGE__SECRET_KEY is not set' <<<"$out" || fail "install-native.sh did not ask for the S3 key: $out"

echo "PASS: every installer refuses placeholder, empty and malformed settings"
