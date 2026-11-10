#!/usr/bin/env bash
# Generate consistent, idempotent secrets + topology-correct endpoints for the
# air-gap roles. Secrets from local `openssl rand` (no network). On the infra
# node, shared values are written identically into .env.infra and .env.server
# so the server reaches its co-located infra by compose service name
# (db/redis/seaweedfs); every other server and worker reaches that infra by
# its LAN address.
set -euo pipefail

# URL-safe alphanumeric secret (base64 stripped of +/=).
envgen_secret() {
  local bytes="${1:-24}"
  openssl rand -base64 "$bytes" | tr -d '/+=\n'
}

# env_get, env_set, postgres_url and redis_url come from release/lib/env.sh.
# shellcheck source=release-lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release-lib.sh"

# envgen_write INFRA SERVER INFRA_EXAMPLE SERVER_EXAMPLE ADMIN_USER ADMIN_PASS
envgen_write() {
  local infra="$1" server="$2" infra_ex="$3" server_ex="$4" admin_user="$5" admin_pass="$6"

  # Seed from the shipped examples so no key is dropped.
  [ -f "$infra" ]  || cp "$infra_ex"  "$infra"
  [ -f "$server" ] || cp "$server_ex" "$server"

  # Shared machine secrets: reuse an existing real value, else generate once.
  local pg redis s3a s3s jwt
  pg="$(env_get "$infra" POSTGRES_PASSWORD)";                                   [ -n "$pg" ]    || pg="$(envgen_secret 24)"
  redis="$(env_get "$infra" REDIS_PASSWORD)";                                   [ -n "$redis" ] || redis="$(envgen_secret 24)"
  s3a="$(env_get "$infra" BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY)";      [ -n "$s3a" ]   || s3a="$(envgen_secret 18)"
  s3s="$(env_get "$infra" BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY)";      [ -n "$s3s" ]   || s3s="$(envgen_secret 24)"
  jwt="$(env_get "$server" BROCCOLI__AUTH__JWT_SECRET)";                        [ -n "$jwt" ]   || jwt="$(envgen_secret 36)"

  # --- infra: secrets + service-name endpoints + neutralize any change-me ---
  env_set "$infra" POSTGRES_PASSWORD "$pg"
  env_set "$infra" REDIS_PASSWORD "$redis"
  env_set "$infra" BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY "$s3a"
  env_set "$infra" BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY "$s3s"
  env_set "$infra" BROCCOLI__AUTH__JWT_SECRET "$jwt"
  env_set "$infra" BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT "http://seaweedfs:8333"
  env_set "$infra" BROCCOLI_BOOTSTRAP_ADMIN_USERNAME "$admin_user"
  env_set "$infra" BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD "$admin_pass"

  # --- server: same secrets + service-name endpoints + admin creds ---
  env_set "$server" BROCCOLI__AUTH__JWT_SECRET "$jwt"
  env_set "$server" BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY "$s3a"
  env_set "$server" BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY "$s3s"
  env_set "$server" BROCCOLI__DATABASE__URL "$(postgres_url postgres "$pg" db 5432 broccoli)"
  env_set "$server" BROCCOLI__MQ__URL "$(redis_url "$redis" redis 6379)"
  env_set "$server" BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT "http://seaweedfs:8333"
  env_set "$server" BROCCOLI_BOOTSTRAP_ADMIN_USERNAME "$admin_user"
  env_set "$server" BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD "$admin_pass"
}

# cluster_seed_infra INFRA SIDECAR_ENV
# Pre-seed .env.infra with the build-time cluster secrets so envgen_write's
# reuse path (env_get -> non-empty) adopts them instead of generating fresh.
# No-op when the sidecar is absent (single-host server keeps generating).
cluster_seed_infra() {
  local infra="$1" sidecar="$2" k v
  [ -f "$sidecar" ] || return 0
  [ -f "$infra" ] || : > "$infra"
  for k in POSTGRES_PASSWORD REDIS_PASSWORD \
           BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY \
           BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY; do
    # Adopt the sidecar value ONLY into a slot that is empty or still a
    # change-me placeholder (env_get returns "" for both). A real value already
    # present — e.g. a POSTGRES_PASSWORD a single-host deploy minted and PG then
    # baked into its data volume — must survive: silently replacing it would lock
    # the server out of its own database.
    v="$(env_get "$sidecar" "$k")"
    [ -n "$v" ] && [ -z "$(env_get "$infra" "$k")" ] && env_set "$infra" "$k" "$v"
  done
}

# workergen_write WORKER WORKER_EXAMPLE SIDECAR_ENV SERVER_HOST WORKER_ID
# Render a worker's .env from the shipped example + shared cluster secrets,
# substituting the server LAN host into the DB/MQ/S3 endpoints.
workergen_write() {
  local worker="$1" worker_ex="$2" sidecar="$3" server_host="$4" worker_id="$5"
  [ -n "$server_host" ] || { echo "workergen: server host required (--lan-host or sidecar BROCCOLI_SERVER_HOST)" >&2; return 2; }
  [ -f "$sidecar" ] || { echo "workergen: cluster-secret file not found: $sidecar" >&2; return 2; }
  [ -f "$worker" ] || cp "$worker_ex" "$worker"
  _envgen_infra_endpoints "$worker" "$sidecar" "$server_host" || return 2
  env_set "$worker" BROCCOLI__WORKER__ID "$worker_id"
}

# The settings a server or worker needs to reach the infra node's Postgres,
# Redis and S3. The cluster-secret sidecar and server-join.env both carry
# them under these names.
_ENVGEN_INFRA_KEYS=(POSTGRES_PASSWORD REDIS_PASSWORD
  BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY)
# What every server must share besides that: a token one server signs must
# verify on the others, and each server resets the bootstrap admin to its own
# setting whenever it boots.
_ENVGEN_SERVER_KEYS=(BROCCOLI__AUTH__JWT_SECRET
  BROCCOLI_BOOTSTRAP_ADMIN_USERNAME BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD)

# _envgen_infra_endpoints ENV SECRETS HOST: point ENV's database, MQ and S3
# settings at the infra node HOST, with the credentials in SECRETS.
_envgen_infra_endpoints() {
  local env="$1" secrets="$2" host="$3" k
  for k in "${_ENVGEN_INFRA_KEYS[@]}"; do
    [ -n "$(env_get "$secrets" "$k")" ] || { echo "envgen: $secrets has no $k" >&2; return 2; }
  done
  env_set "$env" BROCCOLI__DATABASE__URL \
    "$(postgres_url postgres "$(env_get "$secrets" POSTGRES_PASSWORD)" "$host" 5432 broccoli)"
  env_set "$env" BROCCOLI__MQ__URL "$(redis_url "$(env_get "$secrets" REDIS_PASSWORD)" "$host" 6379)"
  env_set "$env" BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT "http://${host}:8333"
  env_set "$env" BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY \
    "$(env_get "$secrets" BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY)"
  env_set "$env" BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY \
    "$(env_get "$secrets" BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY)"
}

# joingen_write INFRA SERVER JOIN
# On the infra node: write JOIN (server-join.env), what a further server needs
# to join this deployment: the infra credentials exactly as this node runs
# them, and the settings every server shares. Rewritten on every run, so it
# follows a rotated secret.
joingen_write() {
  local infra="$1" server="$2" join="$3" k
  [ -f "$join" ] || { : > "$join"; chmod 0600 "$join"; }
  for k in "${_ENVGEN_INFRA_KEYS[@]}"; do env_set "$join" "$k" "$(env_get "$infra" "$k")"; done
  for k in "${_ENVGEN_SERVER_KEYS[@]}"; do env_set "$join" "$k" "$(env_get "$server" "$k")"; done
}

# servergen_join SERVER SERVER_EXAMPLE JOIN INFRA_HOST
# On a further server: render .env.server from the shipped example and the
# infra node's server-join.env, with DB/MQ/S3 at INFRA_HOST. Such a server
# runs no infra of its own; BROCCOLI_INFRA_HOST in .env.server tells
# install.sh so on every later run.
servergen_join() {
  local server="$1" server_ex="$2" join="$3" infra_host="$4" k v
  [ -n "$infra_host" ] || { echo "servergen: infra host required (--infra-host)" >&2; return 2; }
  [ -f "$join" ] || { echo "servergen: $join not found; copy compose/server-join.env from the infra node" >&2; return 2; }
  [ -f "$server" ] || cp "$server_ex" "$server"
  _envgen_infra_endpoints "$server" "$join" "$infra_host" || return 2
  for k in "${_ENVGEN_SERVER_KEYS[@]}"; do
    v="$(env_get "$join" "$k")"
    [ -n "$v" ] || { echo "servergen: $join has no $k" >&2; return 2; }
    env_set "$server" "$k" "$v"
  done
  env_set "$server" BROCCOLI_INFRA_HOST "$infra_host"
}

# gatewaygen_write GATEWAY "LAN_HOST ..." "UPSTREAM ..."
# Render a gateway-only node's .env.gateway: the names and addresses
# contestants reach it by (the first is the TLS default), and the servers it
# balances across. install.sh resolves and checks both on every run.
gatewaygen_write() {
  local gateway="$1" upstreams="$3" hosts
  read -r -d "" -a hosts <<< "$2" || true
  [ "${#hosts[@]}" -gt 0 ] || { echo "gatewaygen: at least one LAN host required (--lan-host)" >&2; return 2; }
  [ -n "${upstreams//[[:space:]]/}" ] || { echo "gatewaygen: at least one upstream server required (--upstream)" >&2; return 2; }
  [ -f "$gateway" ] || { : > "$gateway"; chmod 0600 "$gateway"; }
  env_set "$gateway" LAN_HOST "${hosts[0]}"
  env_set "$gateway" LAN_HOST_ALIASES "${hosts[*]:1}"
  env_set "$gateway" BROCCOLI_UPSTREAMS "$(printf '%s' "$upstreams" | tr -s '[:space:]' ' ' | sed 's/^ //; s/ $//')"
}
