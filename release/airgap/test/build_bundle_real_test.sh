#!/usr/bin/env bash
# Opt-in heavy: real full bundle build. Runs ONLY when AIRGAP_REAL_BUILD is set
# AND a container engine is available; otherwise SKIPs (exit 0). Builds images,
# proves each tar loads, proves the CLI is a static musl ELF, and smoke-tests a
# real compose bring-up. Minutes-long + multi-GB by design.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=/dev/null
. "$here/../lib/runtime.sh"
# shellcheck source=/dev/null
. "$here/../lib/envgen.sh"

[ -n "${AIRGAP_REAL_BUILD:-}" ] || { echo "SKIP: set AIRGAP_REAL_BUILD=1 to run the real build test"; exit 0; }
ENGINE="$(runtime_engine)"; [ -n "$ENGINE" ] || { echo "SKIP: no docker/podman"; exit 0; }
COMPOSE="$(runtime_compose "$ENGINE")"; [ -n "$COMPOSE" ] || { echo "SKIP: no compose provider"; exit 0; }

V="realv"
HOST="${AIRGAP_TEST_HOST:-$(ip route get 1 2>/dev/null | awk '{print $7; exit}')}"
[ -n "$HOST" ] || HOST=127.0.0.1
T="$(mktemp -d)"
cleanup() {
  ( cd "$T/broccoli-airgap-$V/compose" 2>/dev/null && \
    $COMPOSE -p broccoli-worker --env-file .env.worker -f docker-compose.worker.yaml.template down -v 2>/dev/null || true )
  ( cd "$T/broccoli-airgap-$V/compose" 2>/dev/null && \
    $COMPOSE -p compose --env-file .env.infra --env-file .env.server \
      -f docker-compose.infra.yaml.template -f docker-compose.server.yaml.template \
      -f docker-compose.server-airgap.yaml.template \
      -f docker-compose.gateway-airgap.yaml.template down -v 2>/dev/null || true )
  rm -rf "$T"
}
trap cleanup EXIT

echo "== real build (this takes minutes) =="
bash "$here/../build-bundle.sh" --version "$V" --output "$T" --lan-host "$HOST"
b="$T/broccoli-airgap-$V"

echo "== every images/*.tar docker-loads =="
for tar in "$b"/images/*.tar; do
  [ -e "$tar" ] || { echo "FAIL: no image tars produced"; exit 1; }
  "$ENGINE" load -i "$tar" >/dev/null || { echo "FAIL: $tar did not load"; exit 1; }
done

echo "== loaded broccoli tags match the staged env refs =="
srv_ref="$(env_get "$b/compose/.env.server.example" BROCCOLI_SERVER_IMAGE)"
wrk_ref="$(env_get "$b/compose/.env.worker.example" BROCCOLI_WORKER_IMAGE)"
"$ENGINE" image inspect "$srv_ref" >/dev/null 2>&1 || { echo "FAIL: server image $srv_ref not loaded"; exit 1; }
"$ENGINE" image inspect "$wrk_ref" >/dev/null 2>&1 || { echo "FAIL: worker image $wrk_ref not loaded"; exit 1; }

echo "== default plugins staged into the bind-mount source AND manifest-covered =="
# The compose templates mount ./plugins:/plugins:ro OVER the image-baked /plugins;
# the bundle must carry the built plugin set there or the server boots with an
# empty registry (nothing judges). build-bundle copies it out of the server image.
for pf in compose/plugins/standard-checkers/plugin.toml \
          compose/plugins/standard-checkers/standard_checkers.wasm; do
  [ -e "$b/$pf" ] || { echo "FAIL: bundle missing default plugin file: $pf (empty /plugins overlay -> no judging)"; exit 1; }
done
# the heavy debug build tree must never ride along
[ ! -e "$b/compose/plugins/batch-evaluator/target" ] \
  || { echo "FAIL: plugin target/ detritus leaked into bundle"; exit 1; }
# Build-only node_modules must never ride along either. The `!plugins/**`
# dockerignore include (needed for the gitignored built .wasm + frontend dist)
# also drags in every plugin frontend's node_modules — ~54k files, ~460MB —
# unless a plugins-specific re-exclude wins LAST (a generic `**/node_modules/**`
# does NOT, once a plugins-wide `!` include is in force). Assert none survived,
# AND that the runtime payload that re-exclude must preserve (built frontend
# dist) did — guarding both an under-exclude (bloat) and an over-exclude (broken
# frontends). This only manifests through the image build, so it lives here.
nmc="$(find "$b/compose/plugins" -type d -name node_modules | wc -l)"
[ "$nmc" -eq 0 ] \
  || { echo "FAIL: $nmc node_modules dir(s) leaked into bundle plugins (~460MB bloat; dockerignore plugins node_modules re-exclude regressed)"; exit 1; }
find "$b/compose/plugins" -path '*/dist/*' -type f | grep -q . \
  || { echo "FAIL: no built plugin frontend dist in bundle — dockerignore over-excluded (dist is runtime-required for plugin frontends)"; exit 1; }
# shipped plugin code is integrity-protected (present in the manifest)
grep -q 'compose/plugins/standard-checkers/plugin.toml' "$b/manifest.sha256" \
  || { echo "FAIL: staged plugins not covered by manifest.sha256 (integrity gap)"; exit 1; }

echo "== CLI is a static musl ELF =="
# A musl static build reports "static-pie linked" (PIE + no interpreter), while a
# classic non-PIE static build reports "statically linked" — accept either. The
# load-bearing property is that NO dynamic interpreter is required (offline hosts
# have no matching loader/libc), so also assert it is not "dynamically linked".
cli_file="$(file "$b/cli/broccoli")"
echo "$cli_file" | grep -qE 'static-pie linked|statically linked' \
  || { echo "FAIL: CLI is not statically linked (need static-pie or classic static): $cli_file"; exit 1; }
echo "$cli_file" | grep -q 'dynamically linked' \
  && { echo "FAIL: CLI is dynamically linked — will not run on an offline host: $cli_file"; exit 1; } || true

echo "== compose smoke: server up + gateway responds =="
bash "$here/../setup.sh" --role server --bundle "$b" --lan-host "$HOST" \
  --admin-user admin --admin-pass smoke-admin-pw --engine "$ENGINE" --non-interactive
# give the gateway a moment; poll for a TLS response. Verify STRICTLY against the
# bundle CA (not -k): $HOST is a bare IP, and a browser/curl connecting by IP
# sends no SNI, so this exercises both the internal-CA trust chain AND the no-SNI
# cert-selection path (default_sni) — the exact case a bare-IP LAN hits.
ok=0
for _ in $(seq 1 30); do
  if curl -fsS --cacert "$b/ca/root.crt" "https://$HOST/" -o /dev/null; then ok=1; break; fi
  sleep 2
done
[ "$ok" = 1 ] || { echo "FAIL: TLS gateway did not serve a CA-trusted response at https://$HOST/ (check default_sni for bare-IP/no-SNI clients)"; exit 1; }

echo "== compose smoke: plugins actually register at boot (not just 'healthy') =="
# A "healthy" server can still be judging-dead: the runtime runs as distroless
# nonroot 65532, whose HOME is not writable in the base image, so wasmtime cannot
# create its plugin cache and EVERY evaluator/checker/hook plugin fails init()
# silently — empty evaluator registry, no problem creatable, nothing judges. The
# healthcheck stays green throughout. Assert the boot actually registered plugins:
# zero activation failures AND at least one contest type registered (both come
# from the plugin init() path the cache bug breaks).
scid="$($ENGINE ps --filter ancestor="$srv_ref" --format '{{.ID}}' | head -1)"
[ -n "$scid" ] || { echo "FAIL: could not locate the running server container to inspect plugin registration"; exit 1; }
slog="$("$ENGINE" logs "$scid" 2>&1)"
echo "$slog" | grep -qiE 'failed to create cache directory|activation failed|init\(\) failed' \
  && { echo "FAIL: server logged plugin activation/cache failures — evaluator registry is empty, nothing will judge"; exit 1; } || true
echo "$slog" | grep -q 'Contest type registered' \
  || { echo "FAIL: no 'Contest type registered' in server log — plugin init() did not run (writable HOME/wasmtime cache?)"; exit 1; }

echo "== compose smoke: worker up + becomes healthy =="
bash "$here/../setup.sh" --role worker --bundle "$b" --lan-host "$HOST" \
  --worker-id smoke-worker --engine "$ENGINE" --non-interactive
whealthy=0
for _ in $(seq 1 30); do
  cid="$( cd "$b/compose" && $COMPOSE -p broccoli-worker --env-file .env.worker -f docker-compose.worker.yaml.template ps -q worker )"
  [ -n "$cid" ] || { sleep 2; continue; }
  st="$("$ENGINE" inspect -f '{{.State.Health.Status}}' "$cid" 2>/dev/null || echo starting)"
  [ "$st" = healthy ] && { whealthy=1; break; }
  sleep 4
done
[ "$whealthy" = 1 ] || { echo "FAIL: worker container did not become healthy"; exit 1; }

echo "PASS: real bundle builds, loads, ships a static CLI, and boots server+worker offline"
