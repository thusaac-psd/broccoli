#!/usr/bin/env bash
# Render every release compose template with `docker compose config`, using
# the env files the installers actually generate. A template that mounts an
# undeclared volume, extends a missing service, or reads a variable no
# installer writes fails here instead of at `up` time on a contest box.
# Every docker-compose.*.yaml.template must be rendered at least once.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
rel="$(cd "$here/.." && pwd)"

if ! docker compose version >/dev/null 2>&1; then
  echo "SKIP: docker compose is not available"
  exit 0
fi

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
cp -r "$rel/." "$T/"
cd "$T"
mkdir -p plugins

fail() { echo "FAIL: $*"; exit 1; }
install_dry() {
  BROCCOLI_DRY_RUN=1 BROCCOLI_SKIP_TIME_CHECK=1 BROCCOLI_NONINTERACTIVE=1 \
    ./install.sh "$@" </dev/null >"$T/install.log" 2>&1 \
    || { cat "$T/install.log"; fail "install.sh $* dry run"; }
}

rendered=" "
# render LABEL COMPOSE_ARGS...: render, and fail on any unset variable.
render() {
  local label="$1" out f prev=""
  shift
  out="$(docker compose "$@" config 2>&1 >/dev/null)" \
    || { printf '%s\n' "$out"; fail "$label does not render"; }
  if printf '%s\n' "$out" | grep -q 'variable is not set'; then
    printf '%s\n' "$out"
    fail "$label reads a variable its env files do not set"
  fi
  for f in "$@"; do
    [ "$prev" = "-f" ] && rendered="$rendered$(basename "$f") "
    prev="$f"
  done
}

# --- release/install.sh roles ---
install_dry infra
render infra --env-file .env.infra -f docker-compose.infra.yaml.template

install_dry server
render server --env-file .env.server -f docker-compose.server.yaml.template

install_dry worker
render worker --env-file .env.worker -f docker-compose.worker.yaml.template

BROCCOLI_UPSTREAMS='10.0.0.21:3000 10.0.0.22:3000' install_dry gateway
render gateway --env-file .env.gateway -f docker-compose.gateway.yaml.template

install_dry single-host
render single-host --env-file .env.single-host \
  -f docker-compose.single-host.object-storage.yaml.template

BROCCOLI_ENV_FILE=.env.single-host-db BROCCOLI__STORAGE__BACKEND=database install_dry single-host
render "single-host (database)" --env-file .env.single-host-db \
  -f docker-compose.single-host.yaml.template

# --- release/airgap roles (env files from the airgap generators) ---
mkdir -p "$T/ag"
# shellcheck source=../airgap/lib/envgen.sh
. "$rel/airgap/lib/envgen.sh"
envgen_write "$T/ag/.env.infra" "$T/ag/.env.server" \
  "$rel/.env.infra.example" "$rel/.env.server.example" admin 'Admin pass #1'
export LAN_HOST=contest.lan BROCCOLI_TLS_CERT=/tmp/server.crt BROCCOLI_TLS_KEY=/tmp/server.key
render "airgap server" --env-file "$T/ag/.env.infra" --env-file "$T/ag/.env.server" \
  -f docker-compose.infra.yaml.template -f docker-compose.server.yaml.template \
  -f docker-compose.gateway-airgap.yaml.template
unset LAN_HOST BROCCOLI_TLS_CERT BROCCOLI_TLS_KEY

printf 'POSTGRES_PASSWORD=p\nREDIS_PASSWORD=r\nBROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY=a\nBROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY=s\n' \
  > "$T/ag/cluster-secrets.env"
workergen_write "$T/ag/.env.worker" "$rel/.env.worker.example" "$T/ag/cluster-secrets.env" 10.0.0.10 worker-1
render "airgap worker" --env-file "$T/ag/.env.worker" -f docker-compose.worker.yaml.template

for f in "$rel"/docker-compose.*.yaml.template; do
  case "$rendered" in
    *" $(basename "$f") "*) ;;
    *) fail "$(basename "$f") is not rendered by this test; add the env files that deploy it" ;;
  esac
done
echo "PASS: every compose template renders with installer-generated env files"
