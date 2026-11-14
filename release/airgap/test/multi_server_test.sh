#!/usr/bin/env bash
# Dry runs of the multi-server roles: an infra node behind a gateway-only
# node, a further server joining that infra node, and the gateway-only node.
# Checks the arguments each accepts, the env each generates, the trust list
# each server gets and the gateway's upstream list, and that re-runs keep them.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
rel="$here/../.."          # release/
setup="$here/../setup.sh"
install="$here/../install.sh"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
fail() { echo "FAIL: $*"; exit 1; }
# shellcheck source=../../lib/env.sh
. "$rel/lib/env.sh"

mkdir -p "$T/bin"
cat > "$T/bin/docker" <<'E'
#!/usr/bin/env bash
case "$1" in info) exit 0;; compose) exit 0;; *) exit 0;; esac
E
chmod +x "$T/bin/docker"
export PATH="$T/bin:$PATH"

# bundle NAME: a fake bundle with the env examples and a valid manifest.
bundle() {
  mkdir -p "$T/$1/compose"
  cp "$rel/.env.infra.example" "$rel/.env.server.example" "$rel/.env.worker.example" "$T/$1/compose/"
  echo img > "$T/$1/images.txt"
  ( cd "$here/.." && . lib/manifest.sh && manifest_generate "$T/$1" )
}
run_setup() { bash "$setup" --non-interactive --dry-run "$@"; }
# plan_get OUTPUT VAR: a variable install.sh --dry-run exports.
plan_get() { ( eval "$(printf '%s\n' "$1" | grep '^export ')"; printf '%s' "${!2-}" ); }

# --- infra node, gateway on its own host ---
bundle infra
out="$(run_setup --role server --bundle "$T/infra" --gateway-host 10.0.0.5 \
  --server-id server-a --admin-pass 'pw #1')" || fail "infra node setup: $out"
sv="$T/infra/compose/.env.server"
[ "$(env_get "$sv" BROCCOLI_GATEWAY_HOSTS)" = 10.0.0.5 ] || fail "infra node did not record its gateway"
[ "$(env_get "$sv" BROCCOLI__SERVER__ID)" = server-a ] || fail "--server-id not written"
printf '%s\n' "$out" | grep -q -- '--lan-host' && fail "a server behind a gateway-only node asked for a TLS host"
[ ! -e "$T/infra.server-secret" ] || fail "fixture: no server-secret dir expected"
join="$T/infra/compose/server-join.env"
[ "$(stat -c %a "$join")" = 600 ] || fail "server-join.env is not private"
for k in BROCCOLI_DB_PASSWORD REDIS_PASSWORD BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY \
         BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY; do
  [ -n "$(env_get "$join" "$k")" ] && [ "$(env_get "$join" "$k")" = "$(env_get "$T/infra/compose/.env.infra" "$k")" ] \
    || fail "server-join.env $k does not match the infra node"
done
for k in BROCCOLI__AUTH__JWT_SECRET BROCCOLI_BOOTSTRAP_ADMIN_USERNAME BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD; do
  [ "$(env_get "$join" "$k")" = "$(env_get "$sv" "$k")" ] || fail "server-join.env $k does not match the infra node"
done
plan="$(bash "$install" --role server --bundle "$T/infra" --dry-run)" || fail "infra node install dry run: $plan"
[ "$(plan_get "$plan" BROCCOLI_GATEWAY_TRUSTED_PROXIES)" = '["10.0.0.5/32"]' ] || fail "infra node trusts more than its gateway"
[ "$(plan_get "$plan" BROCCOLI_HTTP_BIND)" = 0.0.0.0:3000 ] || fail "infra node does not publish :3000 for the gateway"
compose="$(printf '%s\n' "$plan" | sed -n 's/^compose: //p')"
case "$compose" in
  *docker-compose.infra.yaml.template*docker-compose.server-airgap.yaml.template*) ;;
  *) fail "infra node does not compose infra and server: $compose" ;;
esac
case "$compose" in *gateway-airgap*) fail "infra node still runs its own gateway" ;; esac
printf '%s\n' "$plan" | grep -q 'TLS gateway will serve' && fail "infra node planned a TLS leaf"

# --- a further server joins the infra node ---
bundle extra
cp "$join" "$T/extra/compose/server-join.env"
out="$(run_setup --role server --bundle "$T/extra" --infra-host 10.0.0.10 \
  --gateway-host 10.0.0.5 --server-id server-b)" || fail "joining server setup: $out"
xs="$T/extra/compose/.env.server"
[ ! -e "$T/extra/compose/.env.infra" ] || fail "a joining server generated infra settings"
[ "$(env_get "$xs" BROCCOLI__DATABASE__URL)" = "postgres://broccoli:$(env_get "$join" BROCCOLI_DB_PASSWORD)@10.0.0.10:5432/broccoli" ] \
  || fail "joining server database URL: $(env_get "$xs" BROCCOLI__DATABASE__URL)"
[ "$(env_get "$xs" BROCCOLI__MQ__URL)" = "redis://:$(env_get "$join" REDIS_PASSWORD)@10.0.0.10:6379" ] || fail "joining server MQ URL"
[ "$(env_get "$xs" BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT)" = http://10.0.0.10:8333 ] || fail "joining server S3 endpoint"
for k in BROCCOLI__AUTH__JWT_SECRET BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY; do
  [ "$(env_get "$xs" "$k")" = "$(env_get "$sv" "$k")" ] || fail "joining server $k differs from the infra node"
done
[ "$(env_get "$xs" BROCCOLI__SERVER__ID)" = server-b ] || fail "joining server id"
grep -q change-me "$xs" && fail "joining server kept a placeholder"
plan="$(bash "$install" --role server --bundle "$T/extra" --dry-run)" || fail "joining server install dry run: $plan"
[ "$(plan_get "$plan" BROCCOLI_GATEWAY_TRUSTED_PROXIES)" = '["10.0.0.5/32"]' ] || fail "joining server trusts more than its gateway"
compose="$(printf '%s\n' "$plan" | sed -n 's/^compose: //p')"
[ "$compose" = "-p compose --env-file .env.server -f docker-compose.server.yaml.template -f docker-compose.server-airgap.yaml.template up -d --pull never" ] \
  || fail "joining server compose: $compose"
# A re-run with no flags keeps the node's place and id.
run_setup --role server --bundle "$T/extra" --reconfigure >/dev/null || fail "joining server re-run"
[ "$(env_get "$xs" BROCCOLI_INFRA_HOST)" = 10.0.0.10 ] && [ "$(env_get "$xs" BROCCOLI_GATEWAY_HOSTS)" = 10.0.0.5 ] \
  && [ "$(env_get "$xs" BROCCOLI__SERVER__ID)" = server-b ] || fail "a re-run lost the joining server's settings"
# Without the infra node's server-join.env the join is refused up front.
bundle lost
set +e; out="$(run_setup --role server --bundle "$T/lost" --infra-host 10.0.0.10 --gateway-host 10.0.0.5 2>&1)"; rc=$?; set -e
[ "$rc" = 2 ] && printf '%s\n' "$out" | grep -q 'FAIL: a server joining' || fail "a join without server-join.env was not refused ($rc): $out"
# A gateway named by something that is not an address is refused.
set +e; out="$(run_setup --role server --bundle "$T/extra" --reconfigure --gateway-host 'gw;rm' 2>&1)"; rc=$?; set -e
[ "$rc" = 2 ] || fail "a malformed --gateway-host was accepted: $out"

# --- the gateway-only node ---
bundle gw
sec="$T/gw.server-secret"; mkdir -p "$sec"; echo k > "$sec/root.key"
out="$(run_setup --role gateway --bundle "$T/gw" --lan-host contest.lan --lan-host 10.0.0.5 \
  --upstream 10.0.0.10 --upstream localhost:3001)" || fail "gateway setup: $out"
printf '%s\n' "$out" | grep -q 'install.sh --role gateway' || fail "gateway setup does not hand off to the gateway role"
ge="$T/gw/compose/.env.gateway"
[ "$(env_get "$ge" LAN_HOST)" = contest.lan ] && [ "$(env_get "$ge" LAN_HOST_ALIASES)" = 10.0.0.5 ] \
  || fail "gateway LAN hosts: $(cat "$ge")"
[ "$(env_get "$ge" BROCCOLI_UPSTREAMS)" = '10.0.0.10 localhost:3001' ] || fail "gateway upstreams: $(cat "$ge")"
plan="$(bash "$install" --role gateway --bundle "$T/gw" --dry-run 2>"$T/gw.err")" || fail "gateway install dry run: $plan"
# localhost:3001 is this machine, which would see the gateway by its bridge address.
grep -q 'runs on this machine' "$T/gw.err" || fail "no warning about an upstream on the gateway host"
[ "$(plan_get "$plan" BROCCOLI_UPSTREAMS)" = '10.0.0.10:3000 127.0.0.1:3001' ] || fail "gateway upstream list: $(plan_get "$plan" BROCCOLI_UPSTREAMS)"
[ "$(plan_get "$plan" LAN_HOST)" = contest.lan ] && [ "$(plan_get "$plan" LAN_HOST_ALIASES)" = 10.0.0.5 ] || fail "gateway TLS hosts"
[ "$(plan_get "$plan" BROCCOLI_TLS_KEY)" = "$sec/server.key" ] || fail "gateway leaf path"
compose="$(printf '%s\n' "$plan" | sed -n 's/^compose: //p')"
[ "$compose" = "-p broccoli-gateway --env-file .env.gateway -f docker-compose.gateway-airgap.yaml.template up -d --pull never" ] \
  || fail "gateway compose: $compose"
printf '%s\n' "$plan" | grep -q -- "setup.sh --role server --gateway-host '" || fail "gateway does not say how servers trust it"
# Re-running keeps the generated settings.
cp "$ge" "$T/ge.before"
run_setup --role gateway --bundle "$T/gw" --reconfigure >/dev/null || fail "gateway re-run"
cmp -s "$T/ge.before" "$ge" || fail "a gateway re-run changed .env.gateway"
# Anything but HOST[:PORT] would be spliced into the Caddyfile: refused.
set +e; out="$(run_setup --role gateway --bundle "$T/gw" --reconfigure --upstream 'a} b' 2>&1)"; rc=$?; set -e
[ "$rc" = 2 ] || fail "a malformed upstream was accepted: $out"
set +e; out="$(bash "$install" --role gateway --bundle "$T/gw" --lan-host 'a}b' --dry-run 2>&1)"; rc=$?; set -e
[ "$rc" = 2 ] || fail "a malformed LAN host was accepted: $out"
# The gateway holds the TLS material; without it the preflight stops.
bundle gw2
set +e; out="$(run_setup --role gateway --bundle "$T/gw2" --lan-host contest.lan --upstream 10.0.0.10 2>&1)"; rc=$?; set -e
[ "$rc" = 2 ] && printf '%s\n' "$out" | grep -q '^FAIL: server-secret dir' || fail "gateway without TLS material passed preflight: $out"

# --- the single-host server still runs its gateway and trusts only the edge ---
bundle single
mkdir -p "$T/single.server-secret"; echo k > "$T/single.server-secret/root.key"
run_setup --role server --bundle "$T/single" --lan-host contest.lan --admin-pass pw >/dev/null || fail "single-host setup"
plan="$(bash "$install" --role server --bundle "$T/single" --lan-host contest.lan --dry-run)" || fail "single-host dry run: $plan"
[ "$(plan_get "$plan" BROCCOLI_GATEWAY_TRUSTED_PROXIES)" = '["172.31.250.2/32"]' ] || fail "single host trust list"
[ "$(plan_get "$plan" BROCCOLI_HTTP_BIND)" = 127.0.0.1:3000 ] || fail "single host :3000 not on loopback"
printf '%s\n' "$plan" | grep -q 'docker-compose.gateway-airgap.yaml.template' || fail "single host lost its gateway"
case "$(env_get "$T/single/compose/.env.server" BROCCOLI__SERVER__ID)" in
  server-1 | "") fail "a new server kept the example's shared id" ;;
esac

# --- planted gateway or join settings make a contestant bundle not pristine ---
( . "$here/../lib/manifest.sh"
  manifest_no_hostenv "$T/gw" 2>/dev/null && fail "a bundle with .env.gateway passed as pristine"
  manifest_no_hostenv "$T/extra" 2>/dev/null && fail "a bundle with server-join.env passed as pristine"
  [ "$(manifest_verify "$T/extra")" = OK ] || fail "generated env files broke bundle integrity" ) || exit 1

echo "PASS: gateway-only, infra-node and joining-server dry runs: env, trust list, upstreams, re-runs"
