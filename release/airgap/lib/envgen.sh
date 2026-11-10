#!/usr/bin/env bash
# Generate consistent, idempotent secrets + topology-correct endpoints for the
# air-gap server role. Secrets from local `openssl rand` (no network). Shared
# values are written identically into .env.infra and .env.server so the server
# reaches its co-located infra by compose service name (db/redis/seaweedfs).
set -euo pipefail

# URL-safe alphanumeric secret (base64 stripped of +/=).
envgen_secret() {
  local bytes="${1:-24}"
  openssl rand -base64 "$bytes" | tr -d '/+=\n'
}

# env_get, env_set and url_encode come from the env-file helpers every
# installer shares: next to this file in a bundle, release/lib/ in a checkout.
_envgen_lib="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"
[ -f "$_envgen_lib" ] || _envgen_lib="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../lib" && pwd)/env.sh"
# shellcheck source=../../lib/env.sh
. "$_envgen_lib"

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
  env_set "$infra" BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT "http://${BROCCOLI_S3_HOST:-seaweedfs}:8333"
  env_set "$infra" BROCCOLI_BOOTSTRAP_ADMIN_USERNAME "$admin_user"
  env_set "$infra" BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD "$admin_pass"

  # --- server: same secrets + service-name endpoints + admin creds ---
  env_set "$server" BROCCOLI__AUTH__JWT_SECRET "$jwt"
  env_set "$server" BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY "$s3a"
  env_set "$server" BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY "$s3s"
  env_set "$server" BROCCOLI__DATABASE__URL "$(postgres_url postgres "$pg" "${BROCCOLI_DB_HOST:-db}" 5432 broccoli)"
  env_set "$server" BROCCOLI__MQ__URL "$(redis_url "$redis" "${BROCCOLI_MQ_HOST:-redis}" 6379)"
  env_set "$server" BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT "http://${BROCCOLI_S3_HOST:-seaweedfs}:8333"
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
  local pg redis s3a s3s
  pg="$(env_get "$sidecar" POSTGRES_PASSWORD)"
  redis="$(env_get "$sidecar" REDIS_PASSWORD)"
  s3a="$(env_get "$sidecar" BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY)"
  s3s="$(env_get "$sidecar" BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY)"
  [ -n "$pg" ] && [ -n "$redis" ] && [ -n "$s3a" ] && [ -n "$s3s" ] \
    || { echo "workergen: cluster-secret missing a PG/REDIS/S3 key: $sidecar" >&2; return 2; }
  env_set "$worker" BROCCOLI__DATABASE__URL "$(postgres_url postgres "$pg" "$server_host" 5432 broccoli)"
  env_set "$worker" BROCCOLI__MQ__URL "$(redis_url "$redis" "$server_host" 6379)"
  env_set "$worker" BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT "http://${server_host}:8333"
  env_set "$worker" BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY "$s3a"
  env_set "$worker" BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY "$s3s"
  env_set "$worker" BROCCOLI__WORKER__ID "$worker_id"
}
