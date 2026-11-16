#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"
# shellcheck source=lib/plugins.sh
. "$ROOT/lib/plugins.sh"
# shellcheck source=lib/env.sh
. "$ROOT/lib/env.sh"
# shellcheck source=lib/host.sh
. "$ROOT/lib/host.sh"
BUNDLE_VERSION_DEFAULT="v0.3.0"

die() {
  echo "error: $*" >&2
  exit 1
}

usage() {
  cat >&2 <<'EOF'
usage: ./install.sh <role> [role...]

Roles:
  infra       PostgreSQL, Redis, and optional SeaweedFS object storage
  server      one Broccoli HTTP/API server connected to infra
  worker      one Broccoli judge worker connected to infra
  gateway     optional Caddy load balancer for one or more server machines
  single-host rehearsal-only all-in-one install

Each role writes its own env file, so the same extracted bundle directory can
be used for multiple roles on a LAN host:

  .env.infra, .env.server, .env.worker, .env.gateway, .env.single-host

Infra and single-host installs also write connection.env and server-secrets.env.
Copy connection.env into every server/worker bundle directory. Copy
server-secrets.env only into server bundle directories.

Server and worker nodes need BROCCOLI__DATABASE__URL, BROCCOLI__MQ__URL, and
shared storage credentials from the infra node. Use LAN IPs or private cloud
addresses for those URLs.

Set BROCCOLI_DRY_RUN=1 to generate and validate files without starting Docker.

Optional plugins ship in plugins-available/ and load only once enabled. Set
BROCCOLI_PLUGINS to enable some at install time, for example
  BROCCOLI_PLUGINS="codelink-qualifier codelink-bracket" ./install.sh server
or run ./enable-plugin.sh later. ./enable-plugin.sh with no arguments lists
them.
EOF
}

is_interactive() {
  case "${BROCCOLI_NONINTERACTIVE:-false}" in
    1|true|yes) return 1 ;;
  esac
  case "${BROCCOLI_INTERACTIVE:-false}" in
    1|true|yes) return 0 ;;
  esac
  [ -t 0 ]
}

prompt_value() {
  local name prompt default answer
  name="$1"
  prompt="$2"
  default="$3"
  [ -z "${!name:-}" ] || return 0
  is_interactive || return 0

  if [ -n "$default" ]; then
    printf "%s [%s]: " "$prompt" "$default" >&2
  else
    printf "%s: " "$prompt" >&2
  fi
  read -r answer
  if [ -z "$answer" ]; then
    answer="$default"
  fi
  printf -v "$name" '%s' "$answer"
}

prompt_secret() {
  local name prompt default_mode answer confirm
  name="$1"
  prompt="$2"
  default_mode="${3:-required}"
  [ -z "${!name:-}" ] || return 0
  is_interactive || return 0

  if [ "$default_mode" = "generate" ]; then
    printf "%s [leave blank to generate]: " "$prompt" >&2
  else
    printf "%s: " "$prompt" >&2
  fi
  if [ -t 0 ]; then
    stty -echo
  fi
  read -r answer
  if [ -t 0 ]; then
    stty echo
    printf "\n" >&2
  fi

  if [ -z "$answer" ] && [ "$default_mode" = "generate" ]; then
    return 0
  fi
  [ -n "$answer" ] || return 0

  if [ "$default_mode" != "copied" ]; then
    printf "Confirm %s: " "$prompt" >&2
    if [ -t 0 ]; then
      stty -echo
    fi
    read -r confirm
    if [ -t 0 ]; then
      stty echo
      printf "\n" >&2
    fi
    [ "$answer" = "$confirm" ] || die "secret values did not match"
  fi
  printf -v "$name" '%s' "$answer"
}

choose_role_interactive() {
  local answer
  cat >&2 <<'EOF'
Choose this machine's Broccoli role:
  1) infra       PostgreSQL, Redis, optional SeaweedFS
  2) server      HTTP/API server connected to infra
  3) worker      one judge worker connected to infra
  4) gateway     Caddy load balancer for server machines
  5) single-host rehearsal/demo only
EOF
  printf "Role [1]: " >&2
  read -r answer
  case "${answer:-1}" in
    1|infra) ROLE=infra ;;
    2|server) ROLE=server ;;
    3|worker) ROLE=worker ;;
    4|gateway) ROLE=gateway ;;
    5|single-host) ROLE=single-host ;;
    *) die "unknown role selection '$answer'" ;;
  esac
}

run_multiple_roles() {
  local role
  for role in "$@"; do
    echo
    echo "==> installing role '$role'"
    "$0" "$role"
  done
}

is_dry_run() {
  case "${BROCCOLI_DRY_RUN:-false}" in
    1|true|yes) return 0 ;;
    *) return 1 ;;
  esac
}

need() {
  local cmd hint
  cmd="$1"
  hint="${2:-Install $cmd and retry.}"
  command -v "$cmd" >/dev/null 2>&1 || die "$cmd is required. $hint"
}

random_secret() {
  openssl rand -hex 32
}

primary_ip() {
  hostname -I 2>/dev/null | awk '{print $1}' || true
}

# Settle BROCCOLI_INFRA_HOST, the address servers and workers dial for DB, MQ
# and S3. A guess is never silent: several addresses make the installer say
# which one it took, and none at all stops an infra install, since 127.0.0.1
# would point every other machine at itself. Called directly, not in $(...),
# so that die stops the install.
resolve_infra_host() {
  local addrs
  [ -z "${BROCCOLI_INFRA_HOST:-}" ] || return 0
  addrs="$(hostname -I 2>/dev/null || true)"
  # shellcheck disable=SC2086
  set -- $addrs
  if [ "$#" -eq 0 ]; then
    [ "$ROLE" = "single-host" ] || die "cannot detect this machine's LAN address; set BROCCOLI_INFRA_HOST to the address servers and workers will use"
    BROCCOLI_INFRA_HOST=127.0.0.1
    return 0
  fi
  if [ "$#" -gt 1 ]; then
    echo "warning: this machine has several addresses ($*); servers and workers will be told to use $1." >&2
    echo "warning: set BROCCOLI_INFRA_HOST to choose another one." >&2
  fi
  BROCCOLI_INFRA_HOST="$1"
}

# Behind a gateway every request reaches a server from the gateway's address.
# Unless the server trusts the gateway's X-Forwarded-For, login and register
# limits key on that one address and the whole venue shares one budget.
# BROCCOLI_GATEWAY_HOSTS names the gateway(s); trust exactly those.
gateway_trusted_proxies() {
  [ -n "${BROCCOLI_GATEWAY_HOSTS:-}" ] || return 0
  BROCCOLI__SERVER__TRUSTED_PROXIES="$(trusted_proxies_json "$BROCCOLI_GATEWAY_HOSTS")" \
    || die "BROCCOLI_GATEWAY_HOSTS must list IP addresses or CIDRs, separated by spaces"
}

# Tell the operator which address each upstream server sees this gateway as,
# and how to make that server trust it.
print_gateway_trust_hint() {
  local addrs
  addrs="$(gateway_source_addrs "$BROCCOLI_UPSTREAMS")"
  [ -n "$addrs" ] || addrs="<this gateway's LAN address>"
  cat <<EOF
Servers behind this gateway must trust it for X-Forwarded-For, or every
contestant shares one login rate limit. On each server node run:
  BROCCOLI_GATEWAY_HOSTS='$addrs' ./install.sh server
EOF
}

bind_port() {
  local bind
  bind="$1"
  printf '%s' "${bind##*:}"
}

# The seaweedfs service writes the S3 keys into a JSON identity file, so they
# must be plain JSON string content.
require_json_safe() {
  local name
  for name in "$@"; do
    case "${!name:-}" in
      *\"*|*\\*) die "$name cannot contain double quotes or backslashes" ;;
    esac
  done
}

# Each role is its own Compose project (COMPOSE_PROJECT_NAME, broccoli-<role>
# unless the env file names another), so roles installed from one directory
# get separate networks and never treat each other's containers as orphans.
compose() {
  docker compose -p "$COMPOSE_PROJECT_NAME" --env-file "$ENV_FILE" -f "$COMPOSE_FILE" "$@"
}

# Re-running a role from a newer bundle keeps its env file, which still names
# the previous release's images. Point it at the images just loaded from this
# bundle. Infra images are left alone: those are upgraded only deliberately.
refresh_bundled_image_tags() {
  local current variant image
  case "$ROLE" in
    server|single-host)
      [ -z "${loaded_server_image:-}" ] || env_set_if_changed BROCCOLI_SERVER_IMAGE "$loaded_server_image"
      ;;
  esac
  case "$ROLE" in
    worker|single-host)
      current="$(env_get "$ENV_FILE" BROCCOLI_WORKER_IMAGE)"
      case "$current" in
        *-icpc) variant=icpc ;;
        *-full) variant=full ;;
        *-base) variant=base ;;
        *) variant="" ;;
      esac
      case "$variant" in
        icpc) image="${loaded_worker_icpc_image:-${loaded_worker_image:-}}" ;;
        full) image="${loaded_worker_full_image:-}" ;;
        base) image="${loaded_worker_base_image:-}" ;;
        *) image="" ;;
      esac
      [ -z "$image" ] || env_set_if_changed BROCCOLI_WORKER_IMAGE "$image"
      ;;
    gateway)
      [ -z "${loaded_caddy_image:-}" ] || env_set_if_changed CADDY_IMAGE "$loaded_caddy_image"
      ;;
  esac
}

env_set_if_changed() {
  [ "$(env_get "$ENV_FILE" "$1")" != "$2" ] || return 0
  env_set "$ENV_FILE" "$1" "$2"
  echo "updated $1 in $ENV_FILE to $2"
}

start_role() {
  if [ "$ROLE" = "infra" ] && ! using_object_storage; then
    compose up -d db redis
  else
    compose up -d
  fi
}

selected_template_file() {
  if [ "$ROLE" = "single-host" ] && using_object_storage; then
    printf 'docker-compose.single-host.object-storage.yaml.template\n'
  else
    printf '%s\n' "$TEMPLATE_FILE"
  fi
}

load_image() {
  local archive loaded image_tag
  archive="$1"
  loaded="$(gzip -dc "$archive" | docker load)"
  printf '%s\n' "$loaded" >&2
  image_tag="$(printf '%s\n' "$loaded" | awk -F': ' '/Loaded image:/{print $2; exit}')"
  [ -n "$image_tag" ] || die "docker load did not report an image tag for $archive"
  printf '%s\n' "$image_tag"
}

single_host_plans_object_storage() {
  [ "${BROCCOLI__STORAGE__BACKEND:-}" = "object_storage" ] && return 0
  [ -z "${BROCCOLI__STORAGE__BACKEND:-}" ] && [ ! -f "$ENV_FILE" ] && return 0
  [ "$(env_get "$ENV_FILE" BROCCOLI__STORAGE__BACKEND)" = "object_storage" ]
}

load_bundled_images() {
  local basename image loaded
  loaded_server_image=""
  loaded_worker_base_image=""
  loaded_worker_icpc_image=""
  loaded_worker_full_image=""
  loaded_worker_image=""
  loaded_postgres_image=""
  loaded_redis_image=""
  loaded_seaweedfs_image=""
  loaded_caddy_image=""

  [ -d images ] || return 0
  for image in images/*.tar.gz; do
    [ -e "$image" ] || continue
    basename="$(basename "$image")"
    if [ "$ROLE" = "single-host" ]; then
      case "$basename" in
        caddy.tar.gz) continue ;;
        seaweedfs.tar.gz)
          single_host_plans_object_storage || continue
          ;;
      esac
    fi
    case "$ROLE:$basename" in
      infra:postgres.tar.gz|infra:redis.tar.gz|infra:seaweedfs.tar.gz|\
      server:server.tar.gz|worker:worker-base.tar.gz|worker:worker-icpc.tar.gz|\
      worker:worker-full.tar.gz|gateway:caddy.tar.gz|\
      single-host:server.tar.gz|single-host:worker-base.tar.gz|\
      single-host:worker-icpc.tar.gz|single-host:worker-full.tar.gz|single-host:postgres.tar.gz|\
      single-host:redis.tar.gz|single-host:seaweedfs.tar.gz|single-host:caddy.tar.gz)
        echo "loading $image"
        loaded="$(load_image "$image")"
        case "$basename" in
          server.tar.gz) loaded_server_image="$loaded" ;;
          worker-base.tar.gz) loaded_worker_base_image="$loaded" ;;
          worker-icpc.tar.gz) loaded_worker_icpc_image="$loaded"; loaded_worker_image="$loaded" ;;
          worker-full.tar.gz) loaded_worker_full_image="$loaded" ;;
          postgres.tar.gz) loaded_postgres_image="$loaded" ;;
          redis.tar.gz) loaded_redis_image="$loaded" ;;
          seaweedfs.tar.gz) loaded_seaweedfs_image="$loaded" ;;
          caddy.tar.gz) loaded_caddy_image="$loaded" ;;
        esac
        ;;
    esac
  done
}

worker_image_for_variant() {
  local version variant default_repo
  version="$1"
  variant="$2"
  default_repo="ghcr.io/thusaac-psd/broccoli/broccoli-worker"
  case "$variant" in
    base) printf '%s\n' "${loaded_worker_base_image:-${BROCCOLI_WORKER_BASE_IMAGE:-$default_repo:$version-base}}" ;;
    icpc) printf '%s\n' "${loaded_worker_icpc_image:-${loaded_worker_image:-${BROCCOLI_WORKER_ICPC_IMAGE:-$default_repo:$version-icpc}}}" ;;
    full) printf '%s\n' "${loaded_worker_full_image:-${BROCCOLI_WORKER_FULL_IMAGE:-$default_repo:$version-full}}" ;;
    *) die "unknown worker image variant '$variant'" ;;
  esac
}

choose_worker_image_interactive() {
  local version answer custom
  version="$1"
  [ -z "${BROCCOLI_WORKER_IMAGE:-}" ] || return 0
  is_interactive || return 0

  cat >&2 <<'EOF'
Choose worker image:
  1) icpc  C, C++, Java, Python (recommended for normal contests)
  2) full  ICPC plus Node.js, Go, Rust, Pascal, Kotlin
  3) base  isolate sandbox only; use with a custom derived image
  4) custom image tag
EOF
  printf "Worker image [1]: " >&2
  read -r answer
  case "${answer:-1}" in
    1|icpc) BROCCOLI_WORKER_IMAGE="$(worker_image_for_variant "$version" icpc)" ;;
    2|full) BROCCOLI_WORKER_IMAGE="$(worker_image_for_variant "$version" full)" ;;
    3|base) BROCCOLI_WORKER_IMAGE="$(worker_image_for_variant "$version" base)" ;;
    4|custom)
      printf "Custom worker image tag: " >&2
      read -r custom
      [ -n "$custom" ] || die "custom worker image tag is required"
      BROCCOLI_WORKER_IMAGE="$custom"
      ;;
    *) die "unknown worker image selection '$answer'" ;;
  esac
}

choose_storage_backend_interactive() {
  local answer
  [ -z "${BROCCOLI__STORAGE__BACKEND:-}" ] || return 0
  is_interactive || return 0

  cat >&2 <<'EOF'
Choose blob storage backend:
  1) object_storage SeaweedFS S3-compatible storage (recommended for any contest;
                    keeps testcase blob traffic off the Postgres connection pool)
  2) database       store uploads/results in PostgreSQL (only for tiny demos with no
                    S3 available; cannot survive contest-scale concurrency because
                    every blob fetch holds a DB connection for the entire stream)
EOF
  printf "Storage backend [1]: " >&2
  read -r answer
  case "${answer:-1}" in
    1|object_storage|object|s3|seaweedfs) BROCCOLI__STORAGE__BACKEND=object_storage ;;
    2|database|db) BROCCOLI__STORAGE__BACKEND=database ;;
    *) die "unknown storage backend selection '$answer'" ;;
  esac
}

storage_backend() {
  printf '%s\n' "${BROCCOLI__STORAGE__BACKEND:-object_storage}"
}

using_object_storage() {
  [ "$(storage_backend)" = "object_storage" ]
}

connection_env_file() {
  printf '%s\n' "${BROCCOLI_CONNECTION_ENV_FILE:-connection.env}"
}

server_secrets_env_file() {
  printf '%s\n' "${BROCCOLI_SERVER_SECRETS_ENV_FILE:-server-secrets.env}"
}

# The files the infra install hands to this role: connection.env for servers
# and workers, plus server-secrets.env for servers. Prints those present.
shared_env_files() {
  local file
  case "$ROLE" in
    server) set -- "$(connection_env_file)" "$(server_secrets_env_file)" ;;
    worker) set -- "$(connection_env_file)" ;;
    *) return 0 ;;
  esac
  for file in "$@"; do
    [ -f "$file" ] && printf '%s\n' "$file"
  done
  return 0
}

# Take each setting the infra node handed over, unless the operator set it in
# the environment for this run: the operator's value wins.
load_connection_env_if_present() {
  local file line key
  while IFS= read -r file; do
    echo "loading shared settings from $file"
    while IFS= read -r line; do
      case "$line" in
        [A-Za-z_]*=*) ;;
        *) continue ;;
      esac
      key="${line%%=*}"
      if [ -n "${!key+set}" ]; then
        echo "keeping $key from the environment instead of $file"
        continue
      fi
      export "$key=$(env_unquote "${line#*=}")"
    done < "$file"
  done < <(shared_env_files)
}

# The shared settings that must follow the infra node on a re-run: where its
# database, MQ and object storage are and how to log in to them, and what
# every server must agree on. Anything else in the shared files, such as
# BROCCOLI__AUTH__SECURE_COOKIES, is a per-node choice a re-run never reverts.
SHARED_FOLLOW_KEYS=(
  BROCCOLI__DATABASE__URL BROCCOLI__MQ__URL
  BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET
  BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY
  BROCCOLI__AUTH__JWT_SECRET BROCCOLI_BOOTSTRAP_ADMIN_USERNAME BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD
)

# Re-runs keep an existing env file, but whatever must follow the infra node
# does: after a rehearsal moves to the venue and infra is re-installed with
# new addresses, copy each SHARED_FOLLOW_KEYS value into the env file. A value
# the operator sets in the environment for this run wins over the shared file.
refresh_shared_settings() {
  local file key value from
  while IFS= read -r file; do
    for key in "${SHARED_FOLLOW_KEYS[@]}"; do
      grep -qE "^${key}=" "$file" || continue
      if [ -n "${!key+set}" ]; then
        value="${!key}" from="the environment"
      else
        value="$(env_get "$file" "$key")" from="$file"
      fi
      [ -n "$value" ] || continue
      [ "$(env_get "$ENV_FILE" "$key")" = "$value" ] && continue
      env_set "$ENV_FILE" "$key" "$value"
      echo "updated $key in $ENV_FILE from $from"
    done
  done < <(shared_env_files)
}

# env_stored KEY DEFAULT: KEY from this role's env file, or DEFAULT.
env_stored() { local v; v="$(env_get "$ENV_FILE" "$1")"; printf '%s' "${v:-$2}"; }

# An infra (or single-host) re-run with BROCCOLI_INFRA_HOST given moves the
# stored database, MQ and S3 addresses to that host, keeping the stored
# credentials and ports, so the connection.env it writes points servers and
# workers at the new address (a rehearsal moving to the venue LAN).
rehost_infra_urls() {
  local host="$BROCCOLI_INFRA_HOST" pg_pw redis_pw key value
  pg_pw="$(env_get "$ENV_FILE" POSTGRES_PASSWORD)"
  redis_pw="$(env_get "$ENV_FILE" REDIS_PASSWORD)"
  [ -n "$pg_pw" ] && [ -n "$redis_pw" ] || return 0
  for key in BROCCOLI__DATABASE__URL BROCCOLI__MQ__URL BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT; do
    case "$key" in
      *DATABASE*) value="$(postgres_url "$(env_stored POSTGRES_USER postgres)" "$pg_pw" "$host" \
        "$(bind_port "$(env_stored POSTGRES_BIND 0.0.0.0:5432)")" "$(env_stored POSTGRES_DB broccoli)")" ;;
      *MQ*) value="$(redis_url "$redis_pw" "$host" "$(bind_port "$(env_stored REDIS_BIND 0.0.0.0:6379)")")" ;;
      *)
        [ "$(env_stored BROCCOLI__STORAGE__BACKEND object_storage)" = object_storage ] || continue
        value="http://${host}:$(bind_port "$(env_stored SEAWEEDFS_S3_BIND 0.0.0.0:8333)")"
        # Leave an operator's own S3 endpoint alone; move only the bundled one.
        case "$(env_get "$ENV_FILE" "$key")" in
          "" | "http://"*":${value##*:}") ;;
          *) continue ;;
        esac
        ;;
    esac
    [ "$(env_get "$ENV_FILE" "$key")" = "$value" ] && continue
    env_set "$ENV_FILE" "$key" "$value"
    echo "updated $key in $ENV_FILE for infra host $host"
  done
}

# Single-host containers reach db and redis by service name. Compose cannot
# percent-encode, so the in-container URLs are built here from the stored
# credentials and the template only references them; a password holding `@`,
# `#` or `/` would otherwise break the URL. Rebuilt on every run, which also
# fills them in for env files from before they existed.
single_host_local_urls() {
  local pg_pw redis_pw user db key value
  pg_pw="$(env_get "$ENV_FILE" POSTGRES_PASSWORD)"
  redis_pw="$(env_get "$ENV_FILE" REDIS_PASSWORD)"
  user="$(env_get "$ENV_FILE" POSTGRES_USER)"
  db="$(env_get "$ENV_FILE" POSTGRES_DB)"
  for key in BROCCOLI_LOCAL_DATABASE_URL BROCCOLI_LOCAL_MQ_URL; do
    case "$key" in
      *DATABASE*) value="$(postgres_url "${user:-postgres}" "$pg_pw" db 5432 "${db:-broccoli}")" ;;
      *) value="$(redis_url "$redis_pw" redis 6379)" ;;
    esac
    [ "$(env_get "$ENV_FILE" "$key")" = "$value" ] || env_set "$ENV_FILE" "$key" "$value"
  done
}

print_env_review() {
  local file
  file="$1"
  echo
  echo "Review generated settings in $file:"
  grep -E '^(BROCCOLI_ROLE|BROCCOLI__SERVER__ID|BROCCOLI__WORKER__ID|BROCCOLI_HTTP_BIND|BROCCOLI_GATEWAY_HTTP_BIND|POSTGRES_BIND|REDIS_BIND|BROCCOLI__DATABASE__URL|BROCCOLI__MQ__URL|BROCCOLI__STORAGE__BACKEND|BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT|BROCCOLI_WORKER_IMAGE|BROCCOLI_UPSTREAMS)=' "$file" 2>/dev/null || true
  echo
}

review_env_file_interactive() {
  local answer editor
  is_interactive || return 0
  while true; do
    print_env_review "$ENV_FILE"
    printf "Continue with these settings? [Y/e/q] " >&2
    read -r answer
    case "${answer:-Y}" in
      y|Y|yes|YES) return 0 ;;
      e|E|edit|EDIT)
        editor="${EDITOR:-vi}"
        "$editor" "$ENV_FILE"
        ;;
      q|Q|n|N|no|NO) die "aborted before starting containers" ;;
      *) echo "enter Y to continue, e to edit, or q to abort" >&2 ;;
    esac
  done
}

check_clock_sync() {
  local sync remote_date offset_abs offset
  case "${BROCCOLI_SKIP_TIME_CHECK:-false}" in
    1|true|yes) return 0 ;;
  esac

  echo "checking system clock (local UTC: $(date -u '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || date))"

  if command -v timedatectl >/dev/null 2>&1; then
    sync="$(timedatectl show -p NTPSynchronized --value 2>/dev/null || true)"
    case "$sync" in
      yes) echo "time sync: timedatectl reports synchronized" ;;
      no) echo "warning: timedatectl reports NTP is not synchronized" >&2 ;;
      *) echo "time sync: timedatectl is present but did not report NTP status" >&2 ;;
    esac
  elif command -v chronyc >/dev/null 2>&1; then
    if chronyc tracking >/dev/null 2>&1; then
      echo "time sync: chrony is responding"
    else
      echo "warning: chrony is installed but not reporting healthy tracking" >&2
    fi
  elif command -v systemsetup >/dev/null 2>&1; then
    sync="$(systemsetup -getusingnetworktime 2>/dev/null || true)"
    case "$sync" in
      *On*) echo "time sync: macOS network time is enabled" ;;
      *Off*) echo "warning: macOS network time is disabled" >&2 ;;
      *) echo "time sync: macOS network time status was unavailable" >&2 ;;
    esac
  else
    echo "time sync: no timedatectl/chrony check available; continuing" >&2
  fi
  echo "time sync: every server and worker must agree within about a second; offline, serve time from the infra node (docs/operator-runbook.md, Time Sync on an Offline LAN)"

  if grep -qi microsoft /proc/sys/kernel/osrelease 2>/dev/null; then
    echo "warning: WSL detected. Make sure the Windows host clock is synchronized before judging." >&2
  fi

  if command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
    remote_date="$(curl -fsSI --http1.1 --max-time 3 https://www.cloudflare.com 2>/dev/null | awk 'tolower($1)=="date:" {sub(/^Date:[[:space:]]*/,""); print; exit}' || true)"
    if [ -n "$remote_date" ]; then
      offset="$(REMOTE_HTTP_DATE="$remote_date" python3 - <<'PY' 2>/dev/null || true
import datetime, email.utils, os, time
dt = email.utils.parsedate_to_datetime(os.environ["REMOTE_HTTP_DATE"])
if dt.tzinfo is None:
    dt = dt.replace(tzinfo=datetime.timezone.utc)
print(int(time.time() - dt.timestamp()))
PY
)"
      if [ -n "$offset" ]; then
        offset_abs="${offset#-}"
        if [ "$offset_abs" -gt 5 ]; then
          echo "warning: local clock differs from HTTPS Date header by about ${offset}s" >&2
        else
          echo "time sync: HTTPS Date offset is about ${offset}s"
        fi
      fi
    else
      echo "time sync: HTTPS Date check unavailable" >&2
    fi
  fi
}

# health_url BIND: the /healthz URL of a port published on BIND, reached from
# this host (a wildcard bind is reached on loopback).
health_url() {
  local host
  host="${1%:*}"
  case "$host" in
    ""|"0.0.0.0"|"::"|"[::]") host="127.0.0.1" ;;
  esac
  printf 'http://%s:%s/healthz\n' "$host" "$(bind_port "$1")"
}

host_health_url() {
  health_url "${BROCCOLI_HTTP_BIND:-0.0.0.0:3000}"
}

gateway_health_url() {
  health_url "${BROCCOLI_GATEWAY_HTTP_BIND:-0.0.0.0:80}"
}

should_run_stress_smoke() {
  local setting
  setting="${BROCCOLI_RUN_STRESS_SMOKE:-auto}"
  case "$setting" in
    1|true|yes) return 0 ;;
    0|false|no) return 1 ;;
    auto|"")
      [ "$ROLE" = "single-host" ]
      ;;
    *) die "BROCCOLI_RUN_STRESS_SMOKE must be true, false, or auto" ;;
  esac
}

require_env() {
  local name
  for name in "$@"; do
    [ -n "${!name:-}" ] || die "set $name before installing role '$ROLE'"
    # The .env.*.example files mark every value an operator must supply with
    # change-me (a forgeable JWT secret, a phantom infra address, ...).
    case "${!name}" in
      *change-me*) die "$name still holds a change-me placeholder from an example file; set a real value" ;;
    esac
  done
}

validate_role_env() {
  case "$ROLE" in
    infra)
      require_env POSTGRES_USER POSTGRES_PASSWORD POSTGRES_DB REDIS_PASSWORD \
        BROCCOLI__AUTH__JWT_SECRET BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD
      if using_object_storage; then
        require_env BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET \
          BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY \
          BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY
      fi
      ;;
    server)
      require_env BROCCOLI_SERVER_IMAGE BROCCOLI__SERVER__ID BROCCOLI__DATABASE__URL \
        BROCCOLI__MQ__URL BROCCOLI__AUTH__JWT_SECRET BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD
      if using_object_storage; then
        require_env BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT \
          BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY \
          BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY
      fi
      ;;
    worker)
      require_env BROCCOLI_WORKER_IMAGE BROCCOLI__WORKER__ID BROCCOLI__DATABASE__URL \
        BROCCOLI__MQ__URL
      if using_object_storage; then
        require_env BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT \
          BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY \
          BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY
      fi
      ;;
    gateway)
      require_env CADDY_IMAGE BROCCOLI_UPSTREAMS
      ;;
    single-host)
      require_env BROCCOLI_SERVER_IMAGE BROCCOLI_WORKER_IMAGE POSTGRES_USER POSTGRES_PASSWORD \
        POSTGRES_DB REDIS_PASSWORD BROCCOLI__AUTH__JWT_SECRET \
        BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD
      if using_object_storage; then
        require_env BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET \
          BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY \
          BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY
      fi
      ;;
  esac
  if using_object_storage; then
    require_json_safe BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY \
      BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY
  fi
}

populate_connection_defaults() {
  local infra_host postgres_port redis_port s3_port postgres_user postgres_password postgres_db redis_password
  case "$ROLE" in
    infra|single-host) ;;
    *) return 0 ;;
  esac

  resolve_infra_host
  infra_host="$BROCCOLI_INFRA_HOST"
  postgres_port="$(bind_port "${POSTGRES_BIND:-0.0.0.0:5432}")"
  redis_port="$(bind_port "${REDIS_BIND:-0.0.0.0:6379}")"
  s3_port="$(bind_port "${SEAWEEDFS_S3_BIND:-0.0.0.0:8333}")"
  postgres_user="${POSTGRES_USER:?}"
  postgres_password="${POSTGRES_PASSWORD:?}"
  postgres_db="${POSTGRES_DB:?}"
  redis_password="${REDIS_PASSWORD:?}"

  if [ -z "${BROCCOLI__DATABASE__URL:-}" ]; then
    BROCCOLI__DATABASE__URL="$(postgres_url "$postgres_user" "$postgres_password" "$infra_host" "$postgres_port" "$postgres_db")"
  fi
  if [ -z "${BROCCOLI__MQ__URL:-}" ]; then
    BROCCOLI__MQ__URL="$(redis_url "$redis_password" "$infra_host" "$redis_port")"
  fi
  if using_object_storage && [ -z "${BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT:-}" ]; then
    BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT="http://${infra_host}:${s3_port}"
  fi
}

write_connection_env_file() {
  local file
  file="$(connection_env_file)"
  umask 077
  {
    echo "# Worker-safe shared LAN connection settings generated by ./install.sh $ROLE."
    echo "# Copy this file into every server/worker bundle directory before running"
    echo "# ./install.sh server or ./install.sh worker. It intentionally excludes"
    echo "# server-only secrets."
    env_line BROCCOLI__DATABASE__URL "$BROCCOLI__DATABASE__URL"
    env_line BROCCOLI__MQ__URL "$BROCCOLI__MQ__URL"
    env_line BROCCOLI__STORAGE__BACKEND "$(storage_backend)"
    env_line BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET "${BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET:-broccoli-blobs}"
    env_line BROCCOLI__STORAGE__OBJECT_STORAGE__REGION "${BROCCOLI__STORAGE__OBJECT_STORAGE__REGION:-us-east-1}"
    env_line BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT "${BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT:-}"
    env_line BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY "${BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY:-}"
    env_line BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY "${BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY:-}"
    env_line BROCCOLI__STORAGE__OBJECT_STORAGE__PATH_STYLE "${BROCCOLI__STORAGE__OBJECT_STORAGE__PATH_STYLE:-true}"
  } > "$file.tmp"
  mv "$file.tmp" "$file"
  chmod 0600 "$file"
  echo "created $file for server/worker LAN installs"
}

write_server_secrets_env_file() {
  local file
  file="$(server_secrets_env_file)"
  umask 077
  {
    echo "# Server-only secrets generated by ./install.sh $ROLE."
    echo "# Copy this file only into server bundle directories before running"
    echo "# ./install.sh server."
    env_line BROCCOLI__AUTH__JWT_SECRET "${BROCCOLI__AUTH__JWT_SECRET:-}"
    env_line BROCCOLI__AUTH__SECURE_COOKIES "${BROCCOLI__AUTH__SECURE_COOKIES:-false}"
    env_line BROCCOLI_BOOTSTRAP_ADMIN_USERNAME "${BROCCOLI_BOOTSTRAP_ADMIN_USERNAME:-${BROCCOLI_ADMIN_USERNAME:-admin}}"
    env_line BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD "${BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD:-}"
  } > "$file.tmp"
  mv "$file.tmp" "$file"
  chmod 0600 "$file"
  echo "created $file for server LAN installs"
}

write_env_file() {
  local version server_image worker_image postgres_image redis_image seaweedfs_image caddy_image
  version="${BROCCOLI_VERSION:-$BUNDLE_VERSION_DEFAULT}"
  load_connection_env_if_present
  if [ "$ROLE" = "worker" ] || [ "$ROLE" = "single-host" ]; then
    choose_worker_image_interactive "$version"
  fi
  case "$ROLE" in
    infra|server|worker|single-host)
      choose_storage_backend_interactive
      BROCCOLI__STORAGE__BACKEND="${BROCCOLI__STORAGE__BACKEND:-object_storage}"
      ;;
  esac
  server_image="${loaded_server_image:-${BROCCOLI_SERVER_IMAGE:-ghcr.io/thusaac-psd/broccoli/broccoli-server:$version}}"
  worker_image="${BROCCOLI_WORKER_IMAGE:-$(worker_image_for_variant "$version" icpc)}"
  postgres_image="${loaded_postgres_image:-${POSTGRES_IMAGE:-postgres:18-alpine}}"
  redis_image="${loaded_redis_image:-${REDIS_IMAGE:-redis:7-alpine}}"
  seaweedfs_image="${loaded_seaweedfs_image:-${SEAWEEDFS_IMAGE:-chrislusf/seaweedfs:4.15}}"
  caddy_image="${loaded_caddy_image:-${CADDY_IMAGE:-caddy:2-alpine}}"

  umask 077
  case "$ROLE" in
    infra|single-host)
      local infra_host postgres_user postgres_password postgres_db redis_password s3_access s3_secret s3_bucket s3_endpoint jwt_secret admin_user admin_pass postgres_bind redis_bind s3_bind postgres_port redis_port s3_port
      prompt_value BROCCOLI_INFRA_HOST "Infra address that server/worker machines will use" "$(primary_ip)"
      prompt_value POSTGRES_BIND "PostgreSQL bind address" "0.0.0.0:5432"
      prompt_value REDIS_BIND "Redis bind address" "0.0.0.0:6379"
      prompt_value SEAWEEDFS_S3_BIND "SeaweedFS S3 bind address" "0.0.0.0:8333"
      prompt_value SEAWEEDFS_MASTER_BIND "SeaweedFS master bind address" "127.0.0.1:9333"
      prompt_value BROCCOLI_ADMIN_USERNAME "Initial admin username" "admin"
      prompt_secret BROCCOLI_ADMIN_PASSWORD "Initial admin password" generate
      resolve_infra_host
      infra_host="$BROCCOLI_INFRA_HOST"
      postgres_bind="${POSTGRES_BIND:-0.0.0.0:5432}"
      redis_bind="${REDIS_BIND:-0.0.0.0:6379}"
      s3_bind="${SEAWEEDFS_S3_BIND:-0.0.0.0:8333}"
      postgres_port="$(bind_port "$postgres_bind")"
      redis_port="$(bind_port "$redis_bind")"
      s3_port="$(bind_port "$s3_bind")"
      postgres_user="${POSTGRES_USER:-postgres}"
      postgres_password="${POSTGRES_PASSWORD:-$(random_secret)}"
      postgres_db="${POSTGRES_DB:-broccoli}"
      redis_password="${REDIS_PASSWORD:-$(random_secret)}"
      s3_access="${BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY:-$(random_secret)}"
      s3_secret="${BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY:-$(random_secret)}"
      s3_bucket="${BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET:-broccoli-blobs}"
      s3_endpoint="${BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT:-http://${infra_host}:${s3_port}}"
      jwt_secret="${BROCCOLI__AUTH__JWT_SECRET:-$(random_secret)}"
      admin_user="${BROCCOLI_ADMIN_USERNAME:-admin}"
      admin_pass="${BROCCOLI_ADMIN_PASSWORD:-$(random_secret)}"
      {
        env_line BROCCOLI_ROLE "$ROLE"
        env_line COMPOSE_PROJECT_NAME "broccoli-$ROLE"
        env_line BROCCOLI_VERSION "$version"
        env_line BROCCOLI_SERVER_IMAGE "$server_image"
        env_line BROCCOLI_WORKER_IMAGE "$worker_image"
        env_line POSTGRES_IMAGE "$postgres_image"
        env_line REDIS_IMAGE "$redis_image"
        env_line SEAWEEDFS_IMAGE "$seaweedfs_image"
        env_line CADDY_IMAGE "$caddy_image"
        echo
        env_line POSTGRES_BIND "$postgres_bind"
        env_line REDIS_BIND "$redis_bind"
        env_line SEAWEEDFS_S3_BIND "$s3_bind"
        env_line SEAWEEDFS_MASTER_BIND "${SEAWEEDFS_MASTER_BIND:-127.0.0.1:9333}"
        env_line POSTGRES_USER "$postgres_user"
        env_line POSTGRES_PASSWORD "$postgres_password"
        env_line POSTGRES_DB "$postgres_db"
        env_line REDIS_PASSWORD "$redis_password"
        echo
        env_line BROCCOLI__DATABASE__URL "$(postgres_url "$postgres_user" "$postgres_password" "$infra_host" "$postgres_port" "$postgres_db")"
        env_line BROCCOLI__MQ__URL "$(redis_url "$redis_password" "$infra_host" "$redis_port")"
        env_line BROCCOLI__STORAGE__BACKEND "$(storage_backend)"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET "$s3_bucket"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__REGION "${BROCCOLI__STORAGE__OBJECT_STORAGE__REGION:-us-east-1}"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT "$s3_endpoint"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY "$s3_access"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY "$s3_secret"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__PATH_STYLE "${BROCCOLI__STORAGE__OBJECT_STORAGE__PATH_STYLE:-true}"
        echo
        env_line BROCCOLI__AUTH__JWT_SECRET "$jwt_secret"
        env_line BROCCOLI__AUTH__SECURE_COOKIES "${BROCCOLI__AUTH__SECURE_COOKIES:-false}"
        env_line BROCCOLI_BOOTSTRAP_ADMIN_USERNAME "$admin_user"
        env_line BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD "$admin_pass"
        env_line BROCCOLI_HTTP_BIND "${BROCCOLI_HTTP_BIND:-0.0.0.0:3000}"
        env_line BROCCOLI__SERVER__ID "${BROCCOLI__SERVER__ID:-server-1}"
        env_line BROCCOLI__WORKER__ID "${BROCCOLI__WORKER__ID:-worker-1}"
        env_line BROCCOLI__SERVER__TRUSTED_PROXIES "${BROCCOLI__SERVER__TRUSTED_PROXIES:-[]}"
        env_line BROCCOLI__SERVER__RATE_LIMIT_AUTH "${BROCCOLI__SERVER__RATE_LIMIT_AUTH:-true}"
        env_line BROCCOLI__SUBMISSION__RATE_LIMIT_PER_MINUTE "${BROCCOLI__SUBMISSION__RATE_LIMIT_PER_MINUTE:-10000}"
        env_line BROCCOLI__SERVER_DATABASE_MAX_CONNECTIONS "${BROCCOLI__SERVER_DATABASE_MAX_CONNECTIONS:-40}"
        env_line BROCCOLI__WORKER_DATABASE_MAX_CONNECTIONS "${BROCCOLI__WORKER_DATABASE_MAX_CONNECTIONS:-5}"
        env_line BROCCOLI__OBSERVABILITY__LOG_FORMAT "${BROCCOLI__OBSERVABILITY__LOG_FORMAT:-json}"
        env_line BROCCOLI__OBSERVABILITY__LOG_FILTER "${BROCCOLI__OBSERVABILITY__LOG_FILTER:-info}"
      } > "$ENV_FILE.tmp"
      mv "$ENV_FILE.tmp" "$ENV_FILE"
      ;;
    server)
      prompt_value BROCCOLI__SERVER__ID "Server ID (unique per server)" "$(node_id server)"
      prompt_value BROCCOLI_HTTP_BIND "Server HTTP bind address" "0.0.0.0:3000"
      prompt_value BROCCOLI_GATEWAY_HOSTS "Gateway addresses in front of this server, space-separated (blank if none)" ""
      gateway_trusted_proxies
      prompt_value BROCCOLI__DATABASE__URL "PostgreSQL URL from connection.env" ""
      prompt_value BROCCOLI__MQ__URL "Redis URL from connection.env" ""
      if using_object_storage; then
        prompt_value BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT "SeaweedFS/S3 endpoint from connection.env" ""
        prompt_value BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY "SeaweedFS/S3 access key from connection.env" ""
        prompt_secret BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY "SeaweedFS/S3 secret key from connection.env" copied
      fi
      prompt_secret BROCCOLI__AUTH__JWT_SECRET "JWT secret from server-secrets.env" copied
      prompt_secret BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD "Initial admin password from server-secrets.env" copied
      require_env BROCCOLI__DATABASE__URL BROCCOLI__MQ__URL BROCCOLI__AUTH__JWT_SECRET BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD
      if using_object_storage; then
        require_env BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY
      fi
      {
        env_line BROCCOLI_ROLE server
        env_line COMPOSE_PROJECT_NAME broccoli-server
        env_line BROCCOLI_VERSION "$version"
        env_line BROCCOLI_SERVER_IMAGE "$server_image"
        env_line BROCCOLI_HTTP_BIND "${BROCCOLI_HTTP_BIND:-0.0.0.0:3000}"
        env_line BROCCOLI__SERVER__ID "${BROCCOLI__SERVER__ID:-$(node_id server)}"
        env_line BROCCOLI__SERVER__TRUSTED_PROXIES "${BROCCOLI__SERVER__TRUSTED_PROXIES:-[]}"
        env_line BROCCOLI__SERVER__RATE_LIMIT_AUTH "${BROCCOLI__SERVER__RATE_LIMIT_AUTH:-true}"
        env_line BROCCOLI__SERVER_DATABASE_MAX_CONNECTIONS "${BROCCOLI__SERVER_DATABASE_MAX_CONNECTIONS:-40}"
        env_line BROCCOLI__DATABASE__URL "$BROCCOLI__DATABASE__URL"
        env_line BROCCOLI__MQ__URL "$BROCCOLI__MQ__URL"
        env_line BROCCOLI__AUTH__JWT_SECRET "$BROCCOLI__AUTH__JWT_SECRET"
        env_line BROCCOLI__AUTH__SECURE_COOKIES "${BROCCOLI__AUTH__SECURE_COOKIES:-false}"
        env_line BROCCOLI_BOOTSTRAP_ADMIN_USERNAME "${BROCCOLI_BOOTSTRAP_ADMIN_USERNAME:-${BROCCOLI_ADMIN_USERNAME:-admin}}"
        env_line BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD "$BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD"
        env_line BROCCOLI__SUBMISSION__RATE_LIMIT_PER_MINUTE "${BROCCOLI__SUBMISSION__RATE_LIMIT_PER_MINUTE:-10000}"
        env_line BROCCOLI__STORAGE__BACKEND "$(storage_backend)"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET "${BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET:-broccoli-blobs}"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__REGION "${BROCCOLI__STORAGE__OBJECT_STORAGE__REGION:-us-east-1}"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT "${BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT:-}"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY "${BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY:-}"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY "${BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY:-}"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__PATH_STYLE "${BROCCOLI__STORAGE__OBJECT_STORAGE__PATH_STYLE:-true}"
        env_line BROCCOLI__OBSERVABILITY__LOG_FORMAT "${BROCCOLI__OBSERVABILITY__LOG_FORMAT:-json}"
        env_line BROCCOLI__OBSERVABILITY__LOG_FILTER "${BROCCOLI__OBSERVABILITY__LOG_FILTER:-info}"
      } > "$ENV_FILE.tmp"
      mv "$ENV_FILE.tmp" "$ENV_FILE"
      ;;
    worker)
      prompt_value BROCCOLI__WORKER__ID "Worker ID (unique per worker)" "$(node_id worker)"
      prompt_value BROCCOLI__DATABASE__URL "PostgreSQL URL from connection.env" ""
      prompt_value BROCCOLI__MQ__URL "Redis URL from connection.env" ""
      if using_object_storage; then
        prompt_value BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT "SeaweedFS/S3 endpoint from connection.env" ""
        prompt_value BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY "SeaweedFS/S3 access key from connection.env" ""
        prompt_secret BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY "SeaweedFS/S3 secret key from connection.env" copied
      fi
      require_env BROCCOLI__DATABASE__URL BROCCOLI__MQ__URL
      if using_object_storage; then
        require_env BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY
      fi
      {
        env_line BROCCOLI_ROLE worker
        env_line COMPOSE_PROJECT_NAME broccoli-worker
        env_line BROCCOLI_VERSION "$version"
        env_line BROCCOLI_WORKER_IMAGE "$worker_image"
        env_line BROCCOLI__WORKER__ID "${BROCCOLI__WORKER__ID:-$(node_id worker)}"
        env_line BROCCOLI__WORKER_DATABASE_MAX_CONNECTIONS "${BROCCOLI__WORKER_DATABASE_MAX_CONNECTIONS:-5}"
        env_line BROCCOLI__DATABASE__URL "$BROCCOLI__DATABASE__URL"
        env_line BROCCOLI__MQ__URL "$BROCCOLI__MQ__URL"
        env_line BROCCOLI__STORAGE__BACKEND "$(storage_backend)"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET "${BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET:-broccoli-blobs}"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__REGION "${BROCCOLI__STORAGE__OBJECT_STORAGE__REGION:-us-east-1}"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT "${BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT:-}"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY "${BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY:-}"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY "${BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY:-}"
        env_line BROCCOLI__STORAGE__OBJECT_STORAGE__PATH_STYLE "${BROCCOLI__STORAGE__OBJECT_STORAGE__PATH_STYLE:-true}"
        env_line BROCCOLI__OBSERVABILITY__LOG_FORMAT "${BROCCOLI__OBSERVABILITY__LOG_FORMAT:-json}"
        env_line BROCCOLI__OBSERVABILITY__LOG_FILTER "${BROCCOLI__OBSERVABILITY__LOG_FILTER:-info}"
      } > "$ENV_FILE.tmp"
      mv "$ENV_FILE.tmp" "$ENV_FILE"
      ;;
    gateway)
      prompt_value BROCCOLI_UPSTREAMS "Server upstreams, separated by spaces" ""
      prompt_value BROCCOLI_GATEWAY_HTTP_BIND "Gateway HTTP bind address" "0.0.0.0:80"
      require_env BROCCOLI_UPSTREAMS
      {
        env_line BROCCOLI_ROLE gateway
        env_line COMPOSE_PROJECT_NAME broccoli-gateway
        env_line CADDY_IMAGE "$caddy_image"
        env_line BROCCOLI_GATEWAY_HTTP_BIND "${BROCCOLI_GATEWAY_HTTP_BIND:-0.0.0.0:80}"
        env_line BROCCOLI_UPSTREAMS "$BROCCOLI_UPSTREAMS"
      } > "$ENV_FILE.tmp"
      mv "$ENV_FILE.tmp" "$ENV_FILE"
      ;;
  esac
  chmod 0600 "$ENV_FILE"
  echo "created $ENV_FILE for role '$ROLE'"
}

# wait_until LABEL VERB CHECK [ARG...]: run CHECK every 5s until it succeeds,
# for at most 120s, then die with "LABEL did not VERB within 120s". A CHECK
# that sees a final failure dies itself.
wait_until() {
  local label="$1" verb="$2" attempt
  shift 2
  echo "waiting for $label"
  for attempt in $(seq 1 24); do
    "$@" && return 0
    if [ "$attempt" = 24 ]; then
      compose ps
      die "$label did not $verb within 120s"
    fi
    sleep 5
  done
}

http_ok() {
  curl -fsS "$1" >/dev/null 2>&1
}

service_healthy() {
  local container status
  container="$(compose ps -q "$1" || true)"
  [ -n "$container" ] || return 1
  status="$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$container" 2>/dev/null || true)"
  case "$status" in
    healthy|running) return 0 ;;
  esac
  return 1
}

# service_completed SERVICE LABEL: a one-shot service exited 0; dies if it
# exited with anything else.
service_completed() {
  local container status exit_code
  container="$(compose ps -a -q "$1" || true)"
  [ -n "$container" ] || return 1
  status="$(docker inspect --format '{{.State.Status}}' "$container" 2>/dev/null || true)"
  [ "$status" = "exited" ] || return 1
  exit_code="$(docker inspect --format '{{.State.ExitCode}}' "$container" 2>/dev/null || true)"
  [ "$exit_code" = "0" ] && return 0
  compose logs "$1" || true
  die "$2 exited with status $exit_code"
}

wait_http() {
  wait_until "$2" "become healthy" http_ok "$1"
}

wait_service_healthy() {
  wait_until "$2" "become healthy" service_healthy "$1"
}

wait_service_completed() {
  wait_until "$2" complete service_completed "$1" "$2"
}

if [ "$#" -gt 1 ]; then
  run_multiple_roles "$@"
  exit 0
fi

ROLE="${1:-${BROCCOLI_ROLE:-${BROCCOLI_NODE_ROLE:-}}}"
if [ -z "$ROLE" ] && is_interactive; then
  choose_role_interactive
fi
[ -n "$ROLE" ] || { usage; exit 64; }
case "$ROLE" in
  infra|server|worker|gateway|single-host) ;;
  *) usage; die "unknown role '$ROLE'" ;;
esac

COMPOSE_FILE="docker-compose.${ROLE}.yaml"
TEMPLATE_FILE="${COMPOSE_FILE}.template"
ENV_FILE="${BROCCOLI_ENV_FILE:-.env.$ROLE}"
# Env files are written to FILE.tmp and renamed into place, so a refused value
# never leaves a half-written file that a re-run would then preserve.
trap 'rm -f "$ENV_FILE.tmp" "$(connection_env_file).tmp" "$(server_secrets_env_file).tmp"' EXIT

need openssl "Install OpenSSL with your OS package manager."
if ! is_dry_run; then
  need docker "Install Docker Engine from https://docs.docker.com/engine/install/."
  need gzip "Install gzip with your OS package manager."
  if [ "$ROLE" = "server" ] || [ "$ROLE" = "gateway" ] || [ "$ROLE" = "single-host" ]; then
    need curl "Install curl with your OS package manager."
  fi
  docker compose version >/dev/null 2>&1 || die "docker compose is required. Install the Docker Compose plugin from the official Docker docs."
fi

if ! is_dry_run; then
  load_bundled_images
fi

if [ -f "$ENV_FILE" ]; then
  echo "$ENV_FILE already exists; preserving existing secrets"
  refresh_shared_settings
  refresh_bundled_image_tags
  if { [ "$ROLE" = "infra" ] || [ "$ROLE" = "single-host" ]; } && [ -n "${BROCCOLI_INFRA_HOST:-}" ]; then
    rehost_infra_urls
  fi
  if [ "$ROLE" = "server" ] && [ -n "${BROCCOLI_GATEWAY_HOSTS:-}" ]; then
    gateway_trusted_proxies
    env_set_if_changed BROCCOLI__SERVER__TRUSTED_PROXIES "$BROCCOLI__SERVER__TRUSTED_PROXIES"
  fi
else
  write_env_file
fi
[ "$ROLE" != "single-host" ] || single_host_local_urls

review_env_file_interactive

set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

if [ -n "${BROCCOLI_ROLE:-}" ] && [ "$BROCCOLI_ROLE" != "$ROLE" ]; then
  die "$ENV_FILE was created for role '$BROCCOLI_ROLE'; use the matching role or remove $ENV_FILE"
fi
validate_role_env
COMPOSE_PROJECT_NAME="${COMPOSE_PROJECT_NAME:-broccoli-$ROLE}"

populate_connection_defaults

SELECTED_TEMPLATE_FILE="$(selected_template_file)"
[ -f "$SELECTED_TEMPLATE_FILE" ] || die "$SELECTED_TEMPLATE_FILE is missing"
if [ ! -f "$COMPOSE_FILE" ]; then
  cp "$SELECTED_TEMPLATE_FILE" "$COMPOSE_FILE"
elif ! cmp -s "$SELECTED_TEMPLATE_FILE" "$COMPOSE_FILE"; then
  backup_file="${COMPOSE_FILE}.bak.$(date +%Y%m%d%H%M%S)"
  cp "$COMPOSE_FILE" "$backup_file"
  cp "$SELECTED_TEMPLATE_FILE" "$COMPOSE_FILE"
  echo "Updated $COMPOSE_FILE from template; previous file saved as $backup_file"
fi

if [ "$ROLE" = "infra" ] || [ "$ROLE" = "single-host" ]; then
  write_connection_env_file
  write_server_secrets_env_file
fi

case "$ROLE" in
  server|worker|single-host) enable_requested_plugins "$ROOT" ;;
esac

check_clock_sync

if [ "$ROLE" = "gateway" ]; then
  print_gateway_trust_hint
fi

if is_dry_run; then
  echo "dry run complete; Docker containers were not started"
  exit 0
fi

start_role

case "$ROLE" in
  infra)
    wait_service_healthy db "PostgreSQL"
    wait_service_healthy redis "Redis"
    if using_object_storage; then
      wait_service_healthy seaweedfs "SeaweedFS"
      wait_service_completed seaweedfs-init "SeaweedFS bucket init"
    fi
    ;;
  server)
    wait_http "$(host_health_url)" "server health"
    ;;
  worker)
    wait_service_healthy worker "worker"
    ;;
  gateway)
    wait_http "$(gateway_health_url)" "gateway health"
    ;;
  single-host)
    wait_service_healthy db "PostgreSQL"
    wait_service_healthy redis "Redis"
    if using_object_storage; then
      wait_service_healthy seaweedfs "SeaweedFS"
      wait_service_completed seaweedfs-init "SeaweedFS bucket init"
    fi
    wait_http "$(host_health_url)" "server health"
    wait_service_healthy worker "worker"
    ;;
esac

if should_run_stress_smoke; then
  [ -x stress-test/broccoli-stress-test ] || die "stress-test/broccoli-stress-test is missing or not executable"
  case "$ROLE" in
    server|gateway|single-host)
      base_url="$(if [ "$ROLE" = gateway ]; then gateway_health_url; else host_health_url; fi)"
      # A small judged run on bootstrapped A+B fixtures: every submission
      # must come back with its expected verdict. The latency budget is loose
      # because a fresh single-host install judges one case at a time; this
      # checks correctness, not speed. Kept in sync with the
      # stress-test CLI by packages/stress-test/tests/install_smoke_args.rs.
      ./stress-test/broccoli-stress-test \
        --url "${base_url%/healthz}" \
        --admin-username "${BROCCOLI_BOOTSTRAP_ADMIN_USERNAME:-admin}" \
        --admin-password "${BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD:-}" \
        --total 20 \
        --rate 5 \
        --concurrency 5 \
        --p95-budget-ms 60000
      ;;
  esac
fi

cat <<EOF
Broccoli role '$ROLE' is running.
Compose file: $COMPOSE_FILE
Env file: $ENV_FILE
Logs: docker compose -p $COMPOSE_PROJECT_NAME --env-file $ENV_FILE -f $COMPOSE_FILE logs -f
Runbook: docs/operator-runbook.md
EOF
