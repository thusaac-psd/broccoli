#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../lib/envgen.sh"
rel="$here/../.."          # release/
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
infra="$tmp/.env.infra"; server="$tmp/.env.server"

envgen_write "$infra" "$server" \
  "$rel/.env.infra.example" "$rel/.env.server.example" \
  admin 'adminpw123'

# postgres password identical in infra and the server DB URL
pg="$(env_get "$infra" POSTGRES_PASSWORD)"
[ -n "$pg" ] || { echo "FAIL: empty pg password"; exit 1; }
[ "$(env_get "$server" BROCCOLI__DATABASE__URL)" = "postgres://postgres:${pg}@db:5432/broccoli" ] \
  || { echo "FAIL: DB URL password/host mismatch"; exit 1; }
# endpoints default to service names; phantom IP gone
env_get "$server" BROCCOLI__MQ__URL | grep -qE '^redis://:.+@redis:6379$' || { echo "FAIL: MQ endpoint"; exit 1; }
[ "$(env_get "$server" BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT)" = http://seaweedfs:8333 ] || { echo "FAIL: S3 endpoint"; exit 1; }
grep -q '10.0.0.10' "$server" && { echo "FAIL: phantom 10.0.0.10 remains"; exit 1; } || true
# infra file also has service-name endpoint, no phantom IP
[ "$(env_get "$infra" BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT)" = http://seaweedfs:8333 ] || { echo "FAIL: infra S3 endpoint"; exit 1; }
! grep -q '10.0.0.10' "$infra" || { echo "FAIL: phantom 10.0.0.10 in infra"; exit 1; }
# S3 keys consistent across both files
for k in BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY; do
  a="$(env_get "$infra" "$k")"; b="$(env_get "$server" "$k")"
  [ -n "$a" ] && [ "$a" = "$b" ] || { echo "FAIL: $k inconsistent"; exit 1; }
done
# JWT >= 32 chars
jwt="$(env_get "$server" BROCCOLI__AUTH__JWT_SECRET)"
[ "${#jwt}" -ge 32 ] || { echo "FAIL: jwt too short (${#jwt})"; exit 1; }
# admin creds written, single-quoted
grep -qx "BROCCOLI_BOOTSTRAP_ADMIN_USERNAME='admin'" "$server" || { echo "FAIL: admin user"; exit 1; }
grep -qx "BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD='adminpw123'" "$server" || { echo "FAIL: admin pass"; exit 1; }
# no change-me placeholder in either file
grep -q 'change-me' "$infra"  && { echo "FAIL: placeholder in infra"; exit 1; } || true
grep -q 'change-me' "$server" && { echo "FAIL: placeholder in server"; exit 1; } || true

# backslash escape: env_set replace branch must not corrupt values with backslash sequences
# (the bug lived in the awk -v replace path, not append). Test both paths on same file.
esc_test="$tmp/.env.esc"
env_set "$esc_test" TESTKEY 'C:\temp\new'    # append (creates the key)
env_set "$esc_test" TESTKEY 'C:\temp\new2'   # replace — the path that was broken by awk -v
esc_lines="$(grep -c '^TESTKEY=' "$esc_test")"
[ "$esc_lines" = "1" ] || { echo "FAIL: backslash replace split the line"; exit 1; }
esc_val="$(env_get "$esc_test" TESTKEY)"
[ "$esc_val" = 'C:\temp\new2' ] || { echo "FAIL: backslash value round-trip mismatch: $esc_val"; exit 1; }

# idempotent: rerun with same args preserves every byte
cp "$infra" "$tmp/i1"; cp "$server" "$tmp/s1"
envgen_write "$infra" "$server" \
  "$rel/.env.infra.example" "$rel/.env.server.example" admin 'adminpw123'
diff -q "$tmp/i1" "$infra"  >/dev/null || { echo "FAIL: infra not idempotent"; exit 1; }
diff -q "$tmp/s1" "$server" >/dev/null || { echo "FAIL: server not idempotent"; exit 1; }

# --- operator free text (admin user/pass) is written single-quoted, which
#     `docker compose --env-file` and bash both read literally: '$var' is not
#     interpolated and ' #' does not start a comment. ---
dol_infra="$tmp/.env.infra.dol"; dol_server="$tmp/.env.server.dol"
envgen_write "$dol_infra" "$dol_server" \
  "$rel/.env.infra.example" "$rel/.env.server.example" \
  'ad$min' 'Secret$var99 #2026!'
grep -qxF "BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD='Secret\$var99 #2026!'" "$dol_server" \
  || { echo "FAIL: admin password not single-quoted"; grep ADMIN_PASSWORD "$dol_server"; exit 1; }
grep -qxF "BROCCOLI_BOOTSTRAP_ADMIN_USERNAME='ad\$min'" "$dol_server" \
  || { echo "FAIL: admin username not single-quoted"; exit 1; }
[ "$(env_get "$dol_server" BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD)" = 'Secret$var99 #2026!' ] \
  || { echo "FAIL: env_get did not return the logical value"; exit 1; }
cp "$dol_server" "$tmp/ds1"
envgen_write "$dol_infra" "$dol_server" \
  "$rel/.env.infra.example" "$rel/.env.server.example" 'ad$min' 'Secret$var99 #2026!'
diff -q "$tmp/ds1" "$dol_server" >/dev/null || { echo "FAIL: quoted server not idempotent"; exit 1; }
# A value the two readers cannot agree on is refused, never written mangled.
if envgen_write "$tmp/.env.infra.q" "$tmp/.env.server.q" \
  "$rel/.env.infra.example" "$rel/.env.server.example" admin "it's" 2>/dev/null; then
  echo "FAIL: a single quote in the admin password was accepted"; exit 1
fi
# An unquoted value reads as Compose reads it, with '$' doubled.
printf 'PLAIN=Secret$$var99\n' > "$tmp/.env.plain"
[ "$(env_get "$tmp/.env.plain" PLAIN)" = 'Secret$var99' ] || { echo "FAIL: unquoted \$\$ value misread"; exit 1; }
# A password holding URL delimiters is percent-encoded into the URLs.
url_infra="$tmp/.env.infra.url"; url_server="$tmp/.env.server.url"
cp "$rel/.env.infra.example" "$url_infra"
env_set "$url_infra" POSTGRES_PASSWORD 'p@ss/w#rd?%'
envgen_write "$url_infra" "$url_server" "$rel/.env.infra.example" "$rel/.env.server.example" admin adminpw
[ "$(env_get "$url_server" BROCCOLI__DATABASE__URL)" = 'postgres://postgres:p%40ss%2Fw%23rd%3F%25@db:5432/broccoli' ] \
  || { echo "FAIL: DB URL password not percent-encoded"; exit 1; }

# --- fixture: a cluster-secret sidecar ---
cls="$tmp/cluster-secrets.env"
cat > "$cls" <<EOF
POSTGRES_PASSWORD=pgpw
REDIS_PASSWORD=rdpw
BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY=s3acc
BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY=s3sec
BROCCOLI_SERVER_HOST=10.0.0.10
EOF

# --- workergen_write renders correct URLs/creds/host/id ---
wex="$rel/.env.worker.example"
w="$tmp/.env.worker"
workergen_write "$w" "$wex" "$cls" "10.0.0.10" "worker-7"
grep -qx "BROCCOLI__DATABASE__URL='postgres://postgres:pgpw@10.0.0.10:5432/broccoli'" "$w" \
  || { echo "FAIL: worker DATABASE_URL wrong"; cat "$w"; exit 1; }
grep -qx "BROCCOLI__MQ__URL='redis://:rdpw@10.0.0.10:6379'" "$w" \
  || { echo "FAIL: worker MQ_URL wrong"; exit 1; }
grep -qx "BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT='http://10.0.0.10:8333'" "$w" \
  || { echo "FAIL: worker S3 endpoint wrong"; exit 1; }
grep -qx "BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY='s3acc'" "$w" || { echo "FAIL: worker S3 access wrong"; exit 1; }
grep -qx "BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY='s3sec'" "$w" || { echo "FAIL: worker S3 secret wrong"; exit 1; }
grep -qx "BROCCOLI__WORKER__ID='worker-7'" "$w" || { echo "FAIL: worker id wrong"; exit 1; }

# --- workergen_write refuses a missing server host ---
if workergen_write "$tmp/.env.worker2" "$wex" "$cls" "" "worker-1" 2>/dev/null; then
  echo "FAIL: workergen accepted empty server host"; exit 1; fi

# --- cluster_seed_infra makes envgen_write REUSE the sidecar secrets ---
iex="$rel/.env.infra.example"; sex="$rel/.env.server.example"
infra2="$tmp/.env.infra2"; server2="$tmp/.env.server2"
cp "$iex" "$infra2"                       # start from example (has change-me placeholders)
cluster_seed_infra "$infra2" "$cls"
envgen_write "$infra2" "$server2" "$iex" "$sex" "admin" "adminpw"
grep -qx "POSTGRES_PASSWORD='pgpw'" "$infra2" || { echo "FAIL: infra did not reuse sidecar PG pw"; exit 1; }
grep -qx "REDIS_PASSWORD='rdpw'" "$infra2"    || { echo "FAIL: infra did not reuse sidecar redis pw"; exit 1; }
grep -q  'postgres://postgres:pgpw@' "$server2" || { echo "FAIL: server DATABASE_URL did not use sidecar PG pw"; exit 1; }

# --- cluster_seed_infra must NOT clobber a real pre-existing infra credential.
#     (A single-host deploy may already have minted POSTGRES_PASSWORD and had
#     PG initialise its volume with it; a later cluster sidecar silently
#     replacing it would lock the server out of its own database.) A change-me
#     placeholder IS still adopted, so a genuine first cluster deploy works. ---
infra_pre="$tmp/.env.infra.pre"
cp "$iex" "$infra_pre"
env_set "$infra_pre" POSTGRES_PASSWORD 'already-real-pw'   # a real, non-placeholder value
cluster_seed_infra "$infra_pre" "$cls"                     # sidecar carries POSTGRES_PASSWORD=pgpw
[ "$(env_get "$infra_pre" POSTGRES_PASSWORD)" = 'already-real-pw' ] \
  || { echo "FAIL: cluster_seed_infra clobbered an existing real POSTGRES_PASSWORD"; exit 1; }
[ "$(env_get "$infra_pre" REDIS_PASSWORD)" = 'rdpw' ] \
  || { echo "FAIL: cluster_seed_infra did not adopt sidecar REDIS_PASSWORD over the change-me placeholder"; exit 1; }

# --- workergen_write returns 2 when SIDECAR_ENV file does not exist ---
set +e
workergen_write "$tmp/.env.worker3" "$wex" "$tmp/nonexistent-cluster-secrets.env" "10.0.0.10" "worker-1"
rc=$?
set -e
[ "$rc" = "2" ] || { echo "FAIL: workergen_write rc=$rc, expected 2 for missing sidecar file"; exit 1; }

# --- workergen_write returns 2 when the sidecar is missing a required credential key ---
cls_incomplete="$tmp/cluster-secrets-incomplete.env"
cat > "$cls_incomplete" <<EOF
POSTGRES_PASSWORD=pgpw
EOF
set +e
workergen_write "$tmp/.env.worker4" "$wex" "$cls_incomplete" "10.0.0.10" "worker-1"
rc=$?
set -e
[ "$rc" = "2" ] || { echo "FAIL: workergen_write rc=$rc, expected 2 for sidecar missing REDIS/S3 keys"; exit 1; }

# --- a re-run, where every key is already set, succeeds: under set -e a
#     nonzero status would end setup.sh --reconfigure without a word ---
( set -e; cluster_seed_infra "$infra_pre" "$cls"; cluster_seed_infra "$infra_pre" "$cls" ) \
  || { echo "FAIL: cluster_seed_infra fails when the last key is already set"; exit 1; }

# --- cluster_seed_infra is a true no-op when the sidecar is absent ---
infra3="$tmp/.env.infra3"
cp "$iex" "$infra3"
cp "$infra3" "$tmp/.env.infra3.before"
set +e
cluster_seed_infra "$infra3" "$tmp/nonexistent-cluster-secrets.env"
rc=$?
set -e
[ "$rc" = "0" ] || { echo "FAIL: cluster_seed_infra rc=$rc, expected 0 (no-op) when sidecar absent"; exit 1; }
cmp -s "$tmp/.env.infra3.before" "$infra3" || { echo "FAIL: cluster_seed_infra modified .env.infra when sidecar was absent"; exit 1; }

echo "PASS: envgen secrets consistent, endpoints service-named, idempotent, cluster-seed + workergen_write"
