#!/usr/bin/env bash
# seaweedfs-init really creates the bucket, is safe to re-run, and exits
# nonzero when the bucket cannot be created. Runs the infra template's
# seaweedfs + seaweedfs-init services in a throwaway Compose project on
# loopback ports. Offline: skips unless the SeaweedFS image is already local.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
rel="$(cd "$here/.." && pwd)"
image="$(grep -oE -m1 'SEAWEEDFS_IMAGE:-[^}]+' "$rel/docker-compose.infra.yaml.template" | cut -d- -f2-)"

if ! docker compose version >/dev/null 2>&1 || ! docker image inspect "$image" >/dev/null 2>&1; then
  echo "SKIP: needs docker compose and a local $image image"
  exit 0
fi

T="$(mktemp -d)"
project="broccoli-swinit-test-$$"
cat > "$T/.env" <<'EOF'
POSTGRES_USER=postgres
POSTGRES_PASSWORD=unused
POSTGRES_DB=broccoli
REDIS_PASSWORD=unused
BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY=testaccess
BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY=testsecret
BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET=broccoli-blobs
SEAWEEDFS_MASTER_BIND=127.0.0.1:0
SEAWEEDFS_S3_BIND=127.0.0.1:0
EOF
dc() { docker compose -p "$project" --env-file "$T/.env" -f "$rel/docker-compose.infra.yaml.template" "$@"; }
cleanup() { dc down -v >/dev/null 2>&1 || true; rm -rf "$T"; }
trap cleanup EXIT
fail() { echo "FAIL: $*"; dc logs seaweedfs-init 2>/dev/null | tail -n 20; exit 1; }

dc up -d seaweedfs >/dev/null 2>&1 || fail "seaweedfs did not start"
dc run --rm seaweedfs-init >"$T/first.log" 2>&1 || { cat "$T/first.log"; fail "first init failed"; }
grep -q 'bucket broccoli-blobs is ready' "$T/first.log" || fail "first init did not confirm the bucket"
dc run --rm seaweedfs-init >"$T/second.log" 2>&1 || { cat "$T/second.log"; fail "re-running init failed"; }
if dc run --rm -e BUCKET='Not_A_Valid_Bucket' seaweedfs-init >"$T/bad.log" 2>&1; then
  fail "init exited 0 for a bucket that cannot exist"
fi
grep -q 'is missing after s3.bucket.create' "$T/bad.log" || { cat "$T/bad.log"; fail "failed init did not say why"; }
echo "PASS: seaweedfs-init creates and verifies the bucket, and fails loudly"
