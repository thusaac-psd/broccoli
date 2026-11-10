#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
rel="$here/../.."          # release/
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT

# fake bundle with compose examples + valid manifest
mkdir -p "$tmp/bundle/compose"
cp "$rel/.env.infra.example"  "$tmp/bundle/compose/.env.infra.example"
cp "$rel/.env.server.example" "$tmp/bundle/compose/.env.server.example"
echo img > "$tmp/bundle/images.txt"
( cd "$here/.." && . lib/manifest.sh && manifest_generate "$tmp/bundle" )
# server-secret sidecar (sibling of bundle) with root.key
sec="$tmp/bundle.server-secret"; mkdir -p "$sec"; echo k > "$sec/root.key"
# fake WORKING docker
mkdir -p "$tmp/bin"
cat > "$tmp/bin/docker" <<'E'
#!/usr/bin/env bash
case "$1" in info) exit 0;; compose) [ "$2" = version ] && exit 0; exit 0;; *) exit 0;; esac
E
chmod +x "$tmp/bin/docker"

out="$(PATH="$tmp/bin:$PATH" BROCCOLI_SETUP_ADMIN_PASS=adminpw123 \
  bash "$here/../setup.sh" --role server --bundle "$tmp/bundle" \
    --lan-host contest.lan --admin-user admin --server-secret "$sec" \
    --non-interactive --dry-run)"

echo "$out" | grep -q 'engine:  *docker'          || { echo "FAIL: engine not reported"; exit 1; }
echo "$out" | grep -q 'compose:  *docker compose'  || { echo "FAIL: compose not reported"; exit 1; }
echo "$out" | grep -q 'install.sh --role server'   || { echo "FAIL: exec plan missing"; exit 1; }
# env files generated with consistent secrets + service-name endpoints
sv="$tmp/bundle/compose/.env.server"
grep -qE "^BROCCOLI__DATABASE__URL='postgres://postgres:.+@db:5432/broccoli'$" "$sv" || { echo "FAIL: db url"; exit 1; }
grep -q '10.0.0.10' "$sv" && { echo "FAIL: phantom IP remains"; exit 1; } || true
grep -q 'change-me' "$sv" && { echo "FAIL: placeholder remains"; exit 1; } || true
# dry-run started nothing (no compose project dir side effects beyond env files) — implicit
# dry-run must actually gate exec: install.sh's real runtime marker must NOT appear
echo "$out" | grep -q 'TLS gateway will serve' && { echo "FAIL: dry-run reached install.sh"; exit 1; } || true

# default server-secret sidecar convention (no --server-secret passed) must be
# honored by preflight too, not just by install.sh — regression guard for the
# "preflight FAILs on the default sidecar" bug
set +e
out2="$(PATH="$tmp/bin:$PATH" BROCCOLI_SETUP_ADMIN_PASS=adminpw123 \
  bash "$here/../setup.sh" --role server --bundle "$tmp/bundle" \
    --lan-host contest.lan --admin-user admin \
    --reconfigure --non-interactive --dry-run)"
rc2=$?
set -e
[ "$rc2" = 0 ] || { echo "FAIL: default server-secret sidecar not honored by preflight (rc=$rc2)"; echo "$out2"; exit 1; }
echo "$out2" | grep -q 'install.sh --role server' || { echo "FAIL: exec plan missing (default server-secret run)"; exit 1; }

# the DOCUMENTED invocation is `cd <bundle-dir>; ./setup.sh --bundle .` — the
# default sidecar must resolve to the SIBLING dir, not `..server-secret` inside
# the bundle (canonicalization regression guard).
set +e
out3="$(cd "$tmp/bundle" && PATH="$tmp/bin:$PATH" BROCCOLI_SETUP_ADMIN_PASS=adminpw123 \
  bash "$here/../setup.sh" --role server --bundle . \
    --lan-host contest.lan --admin-user admin \
    --reconfigure --non-interactive --dry-run)"
rc3=$?
set -e
[ "$rc3" = 0 ] || { echo "FAIL: --bundle . default sidecar not resolved (rc=$rc3)"; echo "$out3"; exit 1; }
echo "$out3" | grep -q 'install.sh --role server' || { echo "FAIL: --bundle . run did not reach exec plan"; echo "$out3"; exit 1; }

# missing required non-interactive answer (no lan-host) -> exit 2
set +e
# --reconfigure so the pristine gate is skipped and this genuinely exercises the
# missing-lan-host path (env already exists from the run above; without the opt-in
# the gate would exit 2 for a different reason and mask this assertion).
PATH="$tmp/bin:$PATH" BROCCOLI_SETUP_ADMIN_PASS=x \
  bash "$here/../setup.sh" --role server --bundle "$tmp/bundle" \
    --admin-user admin --server-secret "$sec" --reconfigure --non-interactive --dry-run >/dev/null 2>&1
rc=$?
set -e
[ "$rc" = 2 ] || { echo "FAIL: missing --lan-host should exit 2 (rc=$rc)"; exit 1; }

# --- worker role: renders .env.worker from the cluster-secret sidecar ---
cp "$rel/.env.worker.example" "$tmp/bundle/compose/.env.worker.example"
( cd "$here/.." && . lib/manifest.sh && manifest_generate "$tmp/bundle" )   # re-manifest after adding a file
cls="$tmp/bundle.cluster-secret"; mkdir -p "$cls"
cat > "$cls/cluster-secrets.env" <<EOF
POSTGRES_PASSWORD=pgpw
REDIS_PASSWORD=rdpw
BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY=s3acc
BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY=s3sec
BROCCOLI_SERVER_HOST=10.0.0.10
EOF
outw="$(PATH="$tmp/bin:$PATH" \
  bash "$here/../setup.sh" --role worker --bundle "$tmp/bundle" \
    --worker-id worker-9 --non-interactive --dry-run)"
echo "$outw" | grep -q 'install.sh --role worker' || { echo "FAIL: worker exec plan missing"; echo "$outw"; exit 1; }
echo "$outw" | grep -q '.env.worker (generated)' || { echo "FAIL: worker dry-run plan omits env line"; echo "$outw"; exit 1; }
wenv="$tmp/bundle/compose/.env.worker"
grep -qx "BROCCOLI__WORKER__ID='worker-9'" "$wenv" || { echo "FAIL: worker id not rendered"; cat "$wenv"; exit 1; }
grep -qx "BROCCOLI__DATABASE__URL='postgres://postgres:pgpw@10.0.0.10:5432/broccoli'" "$wenv" || { echo "FAIL: worker db url wrong"; cat "$wenv"; exit 1; }
grep -qx "BROCCOLI__MQ__URL='redis://:rdpw@10.0.0.10:6379'" "$wenv" || { echo "FAIL: worker mq url wrong"; cat "$wenv"; exit 1; }
grep -q 'change-me' "$wenv" && { echo "FAIL: worker placeholder remains"; exit 1; } || true
# A server name only this host resolves reaches the worker containers as an IP.
PATH="$tmp/bin:$PATH" bash "$here/../setup.sh" --role worker --bundle "$tmp/bundle" \
  --lan-host localhost --worker-id worker-9 --reconfigure --non-interactive --dry-run >/dev/null
grep -qx "BROCCOLI__MQ__URL='redis://:rdpw@127.0.0.1:6379'" "$wenv" \
  || { echo "FAIL: worker env names the server by a host-only name"; cat "$wenv"; exit 1; }

# --- server role + cluster-secret sidecar: .env.infra must NOT be corrupted ---
# Regression guard: cluster_seed_infra used to run before envgen_write ever
# seeded the full example, so envgen_write's own [ -f ] || cp seed became a
# no-op and .env.infra ended up missing POSTGRES_DB/POSTGRES_USER (the infra
# template has no ${POSTGRES_DB} default, so Postgres would create db
# 'postgres', not 'broccoli').
rm -f "$tmp/bundle/compose/.env.infra" "$tmp/bundle/compose/.env.server"
# create/overwrite explicitly so this scenario is order-independent from the
# worker scenario above
mkdir -p "$tmp/bundle.cluster-secret"
cat > "$tmp/bundle.cluster-secret/cluster-secrets.env" <<EOF
POSTGRES_PASSWORD=pgpw
REDIS_PASSWORD=rdpw
BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY=s3acc
BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY=s3sec
EOF
PATH="$tmp/bin:$PATH" bash "$here/../setup.sh" --role server --bundle "$tmp/bundle" \
  --lan-host 10.0.0.10 --admin-user admin --admin-pass pw123 \
  --non-interactive --dry-run >/dev/null
sinfra="$tmp/bundle/compose/.env.infra"
grep -qx 'POSTGRES_DB=broccoli' "$sinfra" || { echo "FAIL: .env.infra missing POSTGRES_DB after server+cluster-secret run"; cat "$sinfra"; exit 1; }
grep -qx 'POSTGRES_USER=postgres' "$sinfra" || { echo "FAIL: .env.infra missing POSTGRES_USER after server+cluster-secret run"; cat "$sinfra"; exit 1; }
grep -qx "POSTGRES_PASSWORD='pgpw'" "$sinfra" || { echo "FAIL: .env.infra did not adopt cluster-secret POSTGRES_PASSWORD"; cat "$sinfra"; exit 1; }
sserver="$tmp/bundle/compose/.env.server"
grep -q 'postgres://postgres:pgpw@' "$sserver" || { echo "FAIL: .env.server did not reuse cluster-secret POSTGRES_PASSWORD in db url"; cat "$sserver"; exit 1; }

# --- pristine gate: a planted on-host env file (excluded from the manifest, so
# integrity verify can't see it) must HARD-ABORT the server deploy as a tampering
# signal. --reconfigure is the operator's explicit opt-in for a real re-deploy.
# Uses its own isolated bundle so it is order-independent from the runs above. ---
plant="$(mktemp -d)"
mkdir -p "$plant/bundle/compose"
cp "$rel/.env.infra.example"  "$plant/bundle/compose/.env.infra.example"
cp "$rel/.env.server.example" "$plant/bundle/compose/.env.server.example"
echo img > "$plant/bundle/images.txt"
( cd "$here/.." && . lib/manifest.sh && manifest_generate "$plant/bundle" )
psec="$plant/bundle.server-secret"; mkdir -p "$psec"; echo k > "$psec/root.key"
# plant a rogue .env.server AFTER manifesting — the manifest excludes it, so
# integrity stays green while the planted file rides along
printf 'BROCCOLI__DATABASE__URL=postgres://evil\n' > "$plant/bundle/compose/.env.server"
set +e
outp="$(PATH="$tmp/bin:$PATH" BROCCOLI_SETUP_ADMIN_PASS=adminpw123 \
  bash "$here/../setup.sh" --role server --bundle "$plant/bundle" \
    --lan-host contest.lan --admin-user admin --server-secret "$psec" \
    --non-interactive --dry-run 2>&1)"
rcp=$?
set -e
[ "$rcp" = 2 ] || { echo "FAIL: planted .env.server must hard-abort the server deploy (rc=$rcp)"; echo "$outp"; exit 1; }
echo "$outp" | grep -q -- '--reconfigure' || { echo "FAIL: planting abort must point the operator at --reconfigure"; echo "$outp"; exit 1; }
# --reconfigure opts into the re-deploy despite the pre-existing env file
PATH="$tmp/bin:$PATH" BROCCOLI_SETUP_ADMIN_PASS=adminpw123 \
  bash "$here/../setup.sh" --role server --bundle "$plant/bundle" \
    --lan-host contest.lan --admin-user admin --server-secret "$psec" \
    --reconfigure --non-interactive --dry-run >/dev/null \
  || { echo "FAIL: --reconfigure must allow a re-deploy despite existing on-host env"; exit 1; }
rm -rf "$plant"

echo "PASS: setup.sh dry-run wiring + config generation"
