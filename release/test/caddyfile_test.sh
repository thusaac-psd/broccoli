#!/usr/bin/env bash
# Both gateway Caddyfiles health-check upstreams on the shallow /livez and
# retry unreachable upstreams, and both parse with a real Caddy. The Caddy
# check skips unless a Caddy image is already local (offline-safe).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
rel="$(cd "$here/.." && pwd)"
files=("$rel/examples/Caddyfile.gateway" "$rel/airgap/caddy/Caddyfile.airgap")
fail() { echo "FAIL: $*"; exit 1; }

for cf in "${files[@]}"; do
  name="$(basename "$cf")"
  grep -Eq '^[[:space:]]*health_uri[[:space:]]+/livez$' "$cf" || fail "$name does not probe /livez"
  [ "$(grep -Ec '^[[:space:]]*health_uri' "$cf")" = 1 ] || fail "$name has more than one health_uri"
  for directive in health_interval health_timeout health_fails lb_try_duration; do
    grep -Eq "^[[:space:]]*$directive[[:space:]]" "$cf" || fail "$name has no $directive"
  done
done

image=""
for candidate in caddy:2-alpine caddy:2; do
  if docker image inspect "$candidate" >/dev/null 2>&1; then image="$candidate"; break; fi
done
if [ -z "$image" ]; then
  echo "PASS: static checks (SKIP: no local Caddy image for caddy validate)"
  exit 0
fi

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 1 \
  -subj /CN=contest.lan -keyout "$T/server.key" -out "$T/server.crt" >/dev/null 2>&1
chmod 0644 "$T/server.key"
# validate CADDYFILE ALIASES: caddy validate with the gateway's container env.
validate() {
  docker run --rm -v "$1:/etc/caddy/Caddyfile:ro" -v "$T:/tls:ro" \
    -e BROCCOLI_UPSTREAMS='10.0.0.21:3000 10.0.0.22:3000' -e LAN_HOST=contest.lan \
    -e LAN_HOST_ALIASES="$2" -e TLS_CERT=/tls/server.crt -e TLS_KEY=/tls/server.key \
    "$image" caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile >"$T/validate.log" 2>&1 \
    || { cat "$T/validate.log"; fail "$(basename "$1") does not validate (aliases '$2')"; }
}
for cf in "${files[@]}"; do validate "$cf" ""; done
# A gateway reached by several names and addresses serves all of them.
validate "$rel/airgap/caddy/Caddyfile.airgap" "10.0.0.5 gw.contest.lan"
docker run --rm -v "$rel/airgap/caddy/Caddyfile.airgap:/etc/caddy/Caddyfile:ro" \
  -e BROCCOLI_UPSTREAMS=10.0.0.21:3000 -e LAN_HOST=contest.lan -e LAN_HOST_ALIASES=10.0.0.5 \
  -e TLS_CERT=/tls/server.crt -e TLS_KEY=/tls/server.key \
  "$image" caddy adapt --config /etc/caddy/Caddyfile --adapter caddyfile 2>/dev/null \
  | python3 -c '
import json, sys
srv = json.load(sys.stdin)["apps"]["http"]["servers"]["srv0"]
assert srv["routes"][0]["match"][0]["host"] == ["contest.lan", "10.0.0.5"], srv["routes"][0]["match"]
' || fail "Caddyfile.airgap does not serve LAN_HOST_ALIASES"
echo "PASS: gateway Caddyfiles probe /livez and validate with $image"
