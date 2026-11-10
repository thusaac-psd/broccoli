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

# --- release/airgap roles: env files from setup.sh, the compose command and
# exported settings from install.sh --dry-run, rendered exactly as planned ---
mkdir -p "$T/ag"
# shellcheck source=../airgap/lib/envgen.sh
. "$rel/airgap/lib/envgen.sh"
# agb NAME: an airgap bundle laid out as build-bundle.sh stages one.
agb() {
  local b="$T/$1"
  mkdir -p "$b/compose/plugins" "$b/caddy"
  cp "$T"/docker-compose.*.yaml.template "$T"/.env.*.example "$b/compose/"
  cp "$rel/airgap/caddy/Caddyfile.airgap" "$b/caddy/"
  ( . "$rel/airgap/lib/manifest.sh" && manifest_generate "$b" )
  mkdir -p "$b.server-secret"
  echo key > "$b.server-secret/root.key"
}
airgap_setup() {
  bash "$rel/airgap/setup.sh" --non-interactive --dry-run "$@" >"$T/setup.log" 2>&1 \
    || { cat "$T/setup.log"; fail "airgap setup.sh $*"; }
}
# render_plan LABEL INSTALL_ARGS...: render what install.sh would bring up.
# Leaves the plan's settings exported, for checks, until the next call.
plan_vars=()
render_plan() {
  local label="$1" plan dir args
  shift
  [ "${#plan_vars[@]}" -eq 0 ] || unset "${plan_vars[@]}"
  plan="$(bash "$rel/airgap/install.sh" --dry-run "$@" 2>&1)" \
    || { printf '%s\n' "$plan"; fail "airgap install.sh $* --dry-run"; }
  eval "$(printf '%s\n' "$plan" | grep '^export ')"
  read -r -a plan_vars <<< "$(printf '%s\n' "$plan" | sed -n 's/^export \([A-Z_]*\)=.*/\1/p' | tr '\n' ' ')"
  eval "dir=$(printf '%s\n' "$plan" | sed -n 's/^compose dir: //p')"
  eval "args=($(printf '%s\n' "$plan" | sed -n 's/^compose: //p'))"
  args=("${args[@]:0:${#args[@]}-4}")   # drop "up -d --pull never"
  last_render=(--project-directory "$dir" "${args[@]}")
  cd "$dir"
  render "$label" "${args[@]}"
  cd "$T"
}
# planned_env SERVICE KEY: KEY in SERVICE's environment as last rendered.
planned_env() {
  ( cd "${last_render[1]}" && docker compose "${last_render[@]:2}" config --format json 2>/dev/null ) \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["services"][sys.argv[1]]["environment"][sys.argv[2]])' "$1" "$2"
}

# One host: infra, server and the TLS gateway.
agb ag-single
airgap_setup --role server --bundle "$T/ag-single" --lan-host contest.lan --admin-pass 'Admin pass #1'
render_plan "airgap server" --role server --bundle "$T/ag-single" --lan-host contest.lan
# The quoted env-file value reaches the container as itself, ' #1' included.
[ "$(planned_env server BROCCOLI__BOOTSTRAP__ADMIN_PASSWORD)" = 'Admin pass #1' ] || fail "admin password mangled"
# The override wins over .env.server's TRUSTED_PROXIES and trusts the gateway only.
[ "$(planned_env server BROCCOLI__SERVER__TRUSTED_PROXIES)" = '["172.31.250.0/28"]' ] || fail "airgap server trusts more than the gateway"

# Several servers: the infra node, a server joining it, and a gateway-only
# node in front of both.
agb ag-infra
airgap_setup --role server --bundle "$T/ag-infra" --gateway-host 10.0.0.5 --admin-pass 'Admin pass #1'
render_plan "airgap infra node behind a gateway" --role server --bundle "$T/ag-infra"
[ "$(planned_env server BROCCOLI__SERVER__TRUSTED_PROXIES)" = '["10.0.0.5/32"]' ] || fail "infra node trusts more than its gateway"
agb ag-join
cp "$T/ag-infra/compose/server-join.env" "$T/ag-join/compose/"
airgap_setup --role server --bundle "$T/ag-join" --infra-host 10.0.0.10 --gateway-host 10.0.0.5
render_plan "airgap joining server" --role server --bundle "$T/ag-join"
[ "$(planned_env server BROCCOLI__SERVER__TRUSTED_PROXIES)" = '["10.0.0.5/32"]' ] || fail "joining server trusts more than its gateway"
case "$(planned_env server BROCCOLI__DATABASE__URL)" in
  *@10.0.0.10:5432/broccoli) ;;
  *) fail "joining server does not reach the infra node's database" ;;
esac
[ "$(planned_env server BROCCOLI__AUTH__JWT_SECRET)" = "$(env_get "$T/ag-infra/compose/.env.server" BROCCOLI__AUTH__JWT_SECRET)" ] \
  || fail "joining server signs tokens with another secret"
agb ag-gw
airgap_setup --role gateway --bundle "$T/ag-gw" --lan-host contest.lan --lan-host 10.0.0.5 \
  --upstream 10.0.0.10 --upstream 10.0.0.11:3000
render_plan "airgap gateway-only node" --role gateway --bundle "$T/ag-gw"
[ "$(planned_env gateway BROCCOLI_UPSTREAMS)" = '10.0.0.10:3000 10.0.0.11:3000' ] || fail "gateway upstream list"
[ "$(planned_env gateway LAN_HOST_ALIASES)" = 10.0.0.5 ] || fail "gateway LAN host aliases"
[ "${#plan_vars[@]}" -eq 0 ] || unset "${plan_vars[@]}"

printf 'POSTGRES_PASSWORD=p\nREDIS_PASSWORD=r\nBROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY=a\nBROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY=s\n' \
  > "$T/ag/cluster-secrets.env"
workergen_write "$T/ag/.env.worker" "$rel/.env.worker.example" "$T/ag/cluster-secrets.env" 10.0.0.10 worker-1
render "airgap worker" --env-file "$T/ag/.env.worker" -f docker-compose.worker.yaml.template

# Every BROCCOLI__<SECTION>__<KEY> an example env file documents, including
# commented-out optional ones, must reach the container with its value. A key
# the template does not pass is silently dropped, which is how pool sizing
# from .env.server.example never reached a server.
check_passthrough() {
  local example="$1" template="$2" service="$3" envf="$T/passthrough.env"
  sed -E 's/^#(BROCCOLI__[A-Z_]+__[A-Z_]+=)/\1/' "$rel/$example" > "$envf"
  docker compose --env-file "$envf" -f "$template" config --format json 2>/dev/null \
    | python3 -c '
import json, sys
env = json.load(sys.stdin)["services"][sys.argv[2]]["environment"]
bad = []
for line in open(sys.argv[1]):
    key, sep, value = line.strip().partition("=")
    parts = key.split("__")
    if not sep or len(parts) < 3 or parts[0] != "BROCCOLI":
        continue
    value = value.strip("\x27")
    if str(env.get(key)) != value:
        bad.append(f"{key}={value} (container gets {env.get(key)!r})")
if bad:
    sys.exit("dropped or changed: " + "; ".join(bad))
' "$envf" "$service" || fail "$template drops settings documented in $example"
}
check_passthrough .env.server.example docker-compose.server.yaml.template server
check_passthrough .env.worker.example docker-compose.worker.yaml.template worker

for f in "$rel"/docker-compose.*.yaml.template; do
  case "$rendered" in
    *" $(basename "$f") "*) ;;
    *) fail "$(basename "$f") is not rendered by this test; add the env files that deploy it" ;;
  esac
done
echo "PASS: every compose template renders with installer-generated env files"
