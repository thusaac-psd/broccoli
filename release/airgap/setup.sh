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
  echo "Usage: setup.sh --role {server|worker|contestant} --bundle DIR [--lan-host H]" \
       "[--admin-user U] [--admin-pass P] [--engine docker|podman] [--server-secret DIR]" \
       "[--cluster-secret DIR] [--worker-id ID]" \
       "[--reconfigure] [--non-interactive] [--dry-run]"
}

FLAG_NON_INTERACTIVE="" DRY_RUN="" FLAG_RECONFIGURE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --role)          FLAG_ROLE="$2"; shift 2 ;;
    --bundle)        FLAG_BUNDLE="$2"; shift 2 ;;
    --lan-host)      FLAG_LAN_HOST="$2"; shift 2 ;;
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

ROLE="$(answer ROLE 'Role (server/worker/contestant)' '' 1)"
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
if [ -z "$FLAG_RECONFIGURE" ]; then
  case "$ROLE" in
    server)     hostenv=(.env.infra .env.server) ;;
    worker)     hostenv=(.env.worker) ;;
    contestant) hostenv=(.env.infra .env.server .env.worker) ;;
    *)          hostenv=() ;;
  esac
  if [ "${#hostenv[@]}" -gt 0 ]; then
    manifest_no_hostenv "$BUNDLE" "${hostenv[@]}" \
      || { echo "ABORT: on-host env config already present on this bundle. If this is an intentional re-deploy on a host you configured, re-run with --reconfigure; otherwise treat it as tampering and rebuild the bundle from trusted media." >&2; exit 2; }
  fi
fi

# Default server-secret sidecar convention (documented in
# release/docs/airgap-deployment.md, auto-resolved by install.sh): compute it
# once here so preflight and install.sh agree on the same directory.
secret_dir="${FLAG_SERVER_SECRET:-${BUNDLE%/}.server-secret}"
cluster_dir="${FLAG_CLUSTER_SECRET:-${BUNDLE%/}.cluster-secret}"

[ -n "${FLAG_ENGINE:-}" ] && export BROCCOLI_ENGINE="$FLAG_ENGINE"
ENGINE="$(runtime_engine)"
[ -n "$ENGINE" ] || { echo "FAIL: no working docker or podman (install Docker, or Podman on RHEL-family). Detect-and-guide only; runtime is not bundled." >&2; exit 2; }
COMPOSE="$(runtime_compose "$ENGINE")"
[ -n "$COMPOSE" ] || { echo "FAIL: '$ENGINE' present but no compose provider found." >&2; exit 2; }
export BROCCOLI_ENGINE="$ENGINE" COMPOSE

echo "== preflight ($ROLE) =="
preflight_run "$ROLE" "$BUNDLE" "$secret_dir" "$cluster_dir" \
  || { echo "preflight FAILED — resolve the FAIL lines above; nothing deployed." >&2; exit 2; }

INST=( --role "$ROLE" --bundle "$BUNDLE" )

if [ "$ROLE" = server ]; then
  LAN_HOST="$(answer LAN_HOST 'LAN hostname for TLS (e.g. contest.lan)' '' 1)"
  ADMIN_USER="$(answer ADMIN_USER 'Bootstrap admin username' admin 0)"
  ADMIN_PASS="$(answer_secret ADMIN_PASS 'Bootstrap admin password')"
  infra="$BUNDLE/compose/.env.infra"; server="$BUNDLE/compose/.env.server"
  # Seed the full example first: cluster_seed_infra would otherwise create an
  # empty .env.infra, making envgen_write's own [ -f ] || cp seed a no-op and
  # dropping POSTGRES_DB/POSTGRES_USER (the infra template has no ${POSTGRES_DB}
  # default, so Postgres would create db 'postgres', not 'broccoli').
  [ -f "$infra" ] || cp "$BUNDLE/compose/.env.infra.example" "$infra"
  cluster_seed_infra "$infra" "$cluster_dir/cluster-secrets.env"
  envgen_write "$infra" "$server" \
    "$BUNDLE/compose/.env.infra.example" "$BUNDLE/compose/.env.server.example" \
    "$ADMIN_USER" "$ADMIN_PASS"
  # SELinux relabel of bind-mount sources (TLS leaf dir + Caddyfile + plugins) is
  # done by install.sh — the single deploy engine for both setup.sh and standalone
  # invocations, and for both roles — so it is not duplicated here.
  INST+=( --lan-host "$LAN_HOST" )
  [ -n "${FLAG_SERVER_SECRET:-}" ] && INST+=( --server-secret "$FLAG_SERVER_SECRET" )
fi

if [ "$ROLE" = worker ]; then
  WORKER_ID="$(answer WORKER_ID 'Worker id (unique per worker)' "$(node_id worker)" 0)"
  # server LAN host: explicit --lan-host wins, else the sidecar's baked host
  SERVER_HOST="${FLAG_LAN_HOST:-$(env_get "$cluster_dir/cluster-secrets.env" BROCCOLI_SERVER_HOST)}"
  [ -n "$SERVER_HOST" ] || { echo "worker role needs the server LAN host: pass --lan-host, or build the bundle with --lan-host" >&2; exit 2; }
  workergen_write "$BUNDLE/compose/.env.worker" "$BUNDLE/compose/.env.worker.example" \
    "$cluster_dir/cluster-secrets.env" "$SERVER_HOST" "$WORKER_ID"
fi

if [ -n "$DRY_RUN" ]; then
  echo "== plan (dry-run) =="
  echo "engine:  $ENGINE"
  echo "compose: $COMPOSE"
  [ "$ROLE" = server ] && echo "env:     $BUNDLE/compose/.env.infra, .env.server (generated)"
  [ "$ROLE" = worker ] && echo "env:     $BUNDLE/compose/.env.worker (generated)"
  echo "exec:    install.sh ${INST[*]}"
  exit 0
fi

exec bash "$here/install.sh" "${INST[@]}"
