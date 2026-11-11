#!/usr/bin/env bash
# One-click guided installer for an air-gapped broccoli LAN. Interactive by
# default; fully flag/env-drivable (--non-interactive) for tests and a future
# Ansible slice. TARGET-SIDE: no network. Wraps install.sh (the deploy engine).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=/dev/null
. "$here/lib/runtime.sh"
# shellcheck source=/dev/null
. "$here/lib/answers.sh"
# shellcheck source=/dev/null
. "$here/lib/envgen.sh"
# shellcheck source=/dev/null
. "$here/lib/preflight.sh"
# shellcheck source=/dev/null
. "$here/lib/manifest.sh"

usage() {
  echo "Usage: setup.sh --role {server|gateway|worker|contestant} --bundle DIR [--lan-host H ...]" \
       "[--admin-user U] [--admin-pass P] [--engine docker|podman] [--server-secret DIR]" \
       "[--cluster-secret DIR] [--worker-id ID]" \
       "[--server-id ID] [--gateway-host H ...] [--infra-host H] [--server-join FILE]" \
       "[--upstream HOST[:PORT] ...]" \
       "[--reconfigure] [--non-interactive] [--dry-run]"
}

# add_word VAR WORD: append WORD to the space-separated list in VAR, for the
# flags that may repeat.
add_word() { printf -v "$1" '%s' "${!1:+${!1} }$2"; }

FLAG_NON_INTERACTIVE="" DRY_RUN="" FLAG_RECONFIGURE=""
FLAG_LAN_HOST="" FLAG_GATEWAY_HOSTS="" FLAG_UPSTREAMS=""
while [ $# -gt 0 ]; do
  case "$1" in
    --role)          FLAG_ROLE="$2"; shift 2 ;;
    --bundle)        FLAG_BUNDLE="$2"; shift 2 ;;
    --lan-host)      add_word FLAG_LAN_HOST "$2"; shift 2 ;;
    --gateway-host)  add_word FLAG_GATEWAY_HOSTS "$2"; shift 2 ;;
    --upstream)      add_word FLAG_UPSTREAMS "$2"; shift 2 ;;
    --infra-host)    FLAG_INFRA_HOST="$2"; shift 2 ;;
    --server-join)   FLAG_SERVER_JOIN="$2"; shift 2 ;;
    --server-id)     FLAG_SERVER_ID="$2"; shift 2 ;;
    --admin-user)    FLAG_ADMIN_USER="$2"; shift 2 ;;
    --admin-pass)    FLAG_ADMIN_PASS="$2"; shift 2 ;;
    --engine)        FLAG_ENGINE="$2"; shift 2 ;;
    --server-secret) FLAG_SERVER_SECRET="$2"; shift 2 ;;
    --cluster-secret) FLAG_CLUSTER_SECRET="$2"; shift 2 ;;
    --worker-id)      FLAG_WORKER_ID="$2"; shift 2 ;;
    --reconfigure)   FLAG_RECONFIGURE=1; shift ;;
    --non-interactive) FLAG_NON_INTERACTIVE=1; shift ;;
    --dry-run)       DRY_RUN=1; shift ;;
    -h|--help)       usage; exit 0 ;;
    *) echo "unknown arg: $1" >&2; usage; exit 2 ;;
  esac
done
export FLAG_NON_INTERACTIVE

ROLE="$(answer ROLE 'Role (server/gateway/worker/contestant)' '' 1)"
BUNDLE="$(answer BUNDLE 'Bundle dir' '' 1)"
[ -d "$BUNDLE" ] || { echo "bundle dir not found: $BUNDLE" >&2; exit 2; }
BUNDLE="$(cd "$BUNDLE" && pwd)"   # canonicalize so the default sidecar (and
                                  # install.sh) agree on an absolute path

# Pristine-bundle gate (before any env generation). The on-host env files are
# excluded from manifest integrity (they're generated here and hold secrets), so
# a planted one rides a "verified" bundle undetected and would be adopted by the
# generation below. On a freshly-transported bundle this role's env files must
# NOT yet exist; if they do, refuse — either it's tampering, or it's a genuine
# re-deploy the operator opts into with --reconfigure. Role-scoped so a shared
# staging dir where another role already generated its env is not a false alarm.
# (A joining server's server-join.env is copied in by the operator, so it is
# not gated here.)
if [ -z "$FLAG_RECONFIGURE" ]; then
  case "$ROLE" in
    server)     hostenv=(.env.infra .env.server) ;;
    gateway)    hostenv=(.env.gateway) ;;
    worker)     hostenv=(.env.worker) ;;
    contestant) hostenv=() ;;   # every one of them: manifest_no_hostenv's default
    *)          hostenv=(-) ;;
  esac
  if [ "${hostenv[*]}" != - ]; then
    manifest_no_hostenv "$BUNDLE" "${hostenv[@]}" \
      || { echo "ABORT: on-host env config already present on this bundle. If this is an intentional re-deploy on a host you configured, re-run with --reconfigure; otherwise treat it as tampering and rebuild the bundle from trusted media." >&2; exit 2; }
  fi
fi

# Default server-secret sidecar convention (documented in
# release/docs/airgap-deployment.md, auto-resolved by install.sh): compute it
# once here so preflight and install.sh agree on the same directory.
secret_dir="${FLAG_SERVER_SECRET:-${BUNDLE%/}.server-secret}"
cluster_dir="${FLAG_CLUSTER_SECRET:-${BUNDLE%/}.cluster-secret}"
infra="$BUNDLE/compose/.env.infra"; server="$BUNDLE/compose/.env.server"
gateway="$BUNDLE/compose/.env.gateway"; join="$BUNDLE/compose/server-join.env"

# A server's place in a multi-server deployment (see "Multi-server topology" in
# release/docs/airgap-deployment.md): --infra-host joins that infra node
# instead of running infra here, --gateway-host names the gateway-only node(s)
# in front of it. Neither is prompted for; a re-run keeps what .env.server has.
JOIN="" REMOTE_GW=""
if [ "$ROLE" = server ]; then
  INFRA_HOST="$(FLAG_NON_INTERACTIVE=1 answer INFRA_HOST '' "$(env_get "$server" BROCCOLI_INFRA_HOST)" 0)"
  GATEWAY_HOSTS="$(FLAG_NON_INTERACTIVE=1 answer GATEWAY_HOSTS '' "$(env_get "$server" BROCCOLI_GATEWAY_HOSTS)" 0)"
  [ -z "$INFRA_HOST" ] || JOIN="${FLAG_SERVER_JOIN:-$join}"
  if [ -n "$GATEWAY_HOSTS" ]; then
    REMOTE_GW=1
    trust="$(gateway_trust_json "$GATEWAY_HOSTS")" \
      || { echo "--gateway-host must name the gateway by IP address or resolvable name" >&2; exit 2; }
  fi
fi

[ -n "${FLAG_ENGINE:-}" ] && export BROCCOLI_ENGINE="$FLAG_ENGINE"
ENGINE="$(runtime_engine)"
[ -n "$ENGINE" ] || { echo "FAIL: no working docker or podman (install Docker, or Podman on RHEL-family). Detect-and-guide only; runtime is not bundled." >&2; exit 2; }
COMPOSE="$(runtime_compose "$ENGINE")"
[ -n "$COMPOSE" ] || { echo "FAIL: '$ENGINE' present but no compose provider found." >&2; exit 2; }
export BROCCOLI_ENGINE="$ENGINE" COMPOSE

echo "== preflight ($ROLE) =="
preflight_run "$ROLE" "$BUNDLE" "$secret_dir" "$cluster_dir" "$JOIN" "$REMOTE_GW" \
  || { echo "preflight FAILED — resolve the FAIL lines above; nothing deployed." >&2; exit 2; }

INST=( --role "$ROLE" --bundle "$BUNDLE" )

if [ "$ROLE" = server ]; then
  # The TLS gateway runs here unless a gateway-only node fronts this server.
  if [ -z "$REMOTE_GW" ]; then
    LAN_HOST="$(answer LAN_HOST 'LAN hostname(s)/IP(s) for TLS, space-separated (e.g. contest.lan)' '' 1)"
    for h in $LAN_HOST; do INST+=( --lan-host "$h" ); done
    [ -n "${FLAG_SERVER_SECRET:-}" ] && INST+=( --server-secret "$FLAG_SERVER_SECRET" )
  fi
  # Every server needs its own id: two sharing one take each other's results.
  # A re-run keeps the id this node already has.
  if [ -f "$server" ]; then default_id="$(env_get "$server" BROCCOLI__SERVER__ID)"; else default_id="$(node_id server)"; fi
  SERVER_ID="$(answer SERVER_ID 'Server id (unique per server)' "${default_id:-$(node_id server)}" 0)"
  if [ -n "$INFRA_HOST" ]; then
    # A further server: DB/MQ/S3, the JWT secret and the bootstrap admin all
    # come from the infra node's server-join.env. The containers dial the
    # infra by address, since they cannot read this host's /etc/hosts.
    INFRA_ADDR="$(host_address "$INFRA_HOST")"
    [ "$INFRA_ADDR" = "$INFRA_HOST" ] || echo "server reaches the infra node at $INFRA_ADDR ($INFRA_HOST)"
    servergen_join "$server" "$BUNDLE/compose/.env.server.example" "$JOIN" "$INFRA_ADDR" || exit 2
  else
    ADMIN_USER="$(answer ADMIN_USER 'Bootstrap admin username' admin 0)"
    ADMIN_PASS="$(answer_secret ADMIN_PASS 'Bootstrap admin password')"
    # Seed the full example first: cluster_seed_infra would otherwise create an
    # empty .env.infra, making envgen_write's own [ -f ] || cp seed a no-op and
    # dropping POSTGRES_DB/POSTGRES_USER (the infra template has no ${POSTGRES_DB}
    # default, so Postgres would create db 'postgres', not 'broccoli').
    [ -f "$infra" ] || cp "$BUNDLE/compose/.env.infra.example" "$infra"
    cluster_seed_infra "$infra" "$cluster_dir/cluster-secrets.env"
    envgen_write "$infra" "$server" \
      "$BUNDLE/compose/.env.infra.example" "$BUNDLE/compose/.env.server.example" \
      "$ADMIN_USER" "$ADMIN_PASS"
    # What a further server needs to join this one (server hosts only).
    joingen_write "$infra" "$server" "$join"
  fi
  env_set "$server" BROCCOLI__SERVER__ID "$SERVER_ID"
  if [ -n "$REMOTE_GW" ]; then
    env_set "$server" BROCCOLI_GATEWAY_HOSTS "$GATEWAY_HOSTS"
    echo "server will trust forwarded client addresses only from $trust"
  fi
  # SELinux relabel of bind-mount sources (TLS leaf dir + Caddyfile + plugins) is
  # done by install.sh — the single deploy engine for both setup.sh and standalone
  # invocations, and for every role — so it is not duplicated here.
fi

if [ "$ROLE" = gateway ]; then
  # A gateway-only node: the TLS gateway for the servers in --upstream. A
  # re-run keeps what .env.gateway has.
  old_hosts="$(env_get "$gateway" LAN_HOST) $(env_get "$gateway" LAN_HOST_ALIASES)"
  old_hosts="$(printf '%s' "$old_hosts" | sed 's/^ *//; s/ *$//')"
  LAN_HOSTS="$(answer LAN_HOST 'LAN hostname(s)/IP(s) contestants use, space-separated (e.g. contest.lan 10.0.0.5)' "$old_hosts" 1)"
  UPSTREAMS="$(answer UPSTREAMS 'Servers to balance across, HOST[:PORT] space-separated' "$(env_get "$gateway" BROCCOLI_UPSTREAMS)" 1)"
  upstream_list "$UPSTREAMS" >/dev/null || exit 2
  gatewaygen_write "$gateway" "$LAN_HOSTS" "$UPSTREAMS" || exit 2
  [ -n "${FLAG_SERVER_SECRET:-}" ] && INST+=( --server-secret "$FLAG_SERVER_SECRET" )
fi

if [ "$ROLE" = worker ]; then
  # A re-run keeps the id this worker already has.
  default_id="$(env_get "$BUNDLE/compose/.env.worker" BROCCOLI__WORKER__ID)"
  WORKER_ID="$(answer WORKER_ID 'Worker id (unique per worker)' "${default_id:-$(node_id worker)}" 0)"
  # server LAN host: explicit --lan-host wins, else the sidecar's baked host
  SERVER_HOST="${FLAG_LAN_HOST:-$(env_get "$cluster_dir/cluster-secrets.env" BROCCOLI_SERVER_HOST)}"
  [ -n "$SERVER_HOST" ] || { echo "worker role needs the server LAN host: pass --lan-host, or build the bundle with --lan-host" >&2; exit 2; }
  # The worker containers dial DB/MQ/S3 by this address, and they cannot see
  # names that only this host's /etc/hosts knows, so hand them the IP.
  SERVER_ADDR="$(host_address "$SERVER_HOST")"
  [ "$SERVER_ADDR" = "$SERVER_HOST" ] || echo "worker reaches the server at $SERVER_ADDR ($SERVER_HOST)"
  workergen_write "$BUNDLE/compose/.env.worker" "$BUNDLE/compose/.env.worker.example" \
    "$cluster_dir/cluster-secrets.env" "$SERVER_ADDR" "$WORKER_ID"
fi

if [ -n "$DRY_RUN" ]; then
  echo "== plan (dry-run) =="
  echo "engine:  $ENGINE"
  echo "compose: $COMPOSE"
  if [ "$ROLE" = server ] && [ -n "$INFRA_HOST" ]; then
    echo "env:     $BUNDLE/compose/.env.server (generated from $JOIN)"
  elif [ "$ROLE" = server ]; then
    echo "env:     $BUNDLE/compose/.env.infra, .env.server, server-join.env (generated)"
  fi
  [ "$ROLE" = gateway ] && echo "env:     $BUNDLE/compose/.env.gateway (generated)"
  [ "$ROLE" = worker ] && echo "env:     $BUNDLE/compose/.env.worker (generated)"
  echo "exec:    install.sh ${INST[*]}"
  exit 0
fi

exec bash "$here/install.sh" "${INST[@]}"
