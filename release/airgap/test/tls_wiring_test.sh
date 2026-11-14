#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
inst="$here/../install.sh"
cf="$here/../caddy/Caddyfile.airgap"
gw="$here/../../docker-compose.gateway-airgap.yaml.template"
srv="$here/../../docker-compose.server-airgap.yaml.template"

# install.sh must NOT envsubst the Caddyfile (that corrupts Caddy {$VAR}).
grep -q 'envsubst' "$inst" && { echo "FAIL: install.sh still runs envsubst (corrupts Caddy {\$VAR})"; exit 1; } || true
# The server role must compose the TLS gateway template.
grep -q 'docker-compose.gateway-airgap.yaml.template' "$inst" || { echo "FAIL: server install does not compose the TLS gateway"; exit 1; }
grep -q 'docker-compose.server-airgap.yaml.template' "$inst" || { echo "FAIL: server install does not compose the server-airgap override"; exit 1; }
# gateway template exists, publishes 443, mounts the un-rendered Caddyfile.
[ -f "$gw" ] || { echo "FAIL: gateway-airgap compose template missing"; exit 1; }
grep -q ':443' "$gw" || { echo "FAIL: gateway does not publish 443"; exit 1; }
grep -q 'Caddyfile.airgap:/etc/caddy/Caddyfile' "$gw" || { echo "FAIL: gateway does not mount the un-rendered Caddyfile"; exit 1; }
# The CA signing key (root.key) lives in the server-secret dir. The gateway is
# the contestant-facing container, so it must mount ONLY the leaf cert+key as
# individual files — never the whole secret dir (which would expose root.key).
grep -q 'BROCCOLI_TLS_DIR' "$gw" \
  && { echo "FAIL: gateway bind-mounts the whole TLS dir — leaks CA root.key into the Caddy container"; exit 1; } || true
grep -q '/etc/broccoli-tls/server.crt:ro' "$gw" || { echo "FAIL: gateway does not mount server.crt as an individual file"; exit 1; }
grep -q '/etc/broccoli-tls/server.key:ro' "$gw" || { echo "FAIL: gateway does not mount server.key as an individual file"; exit 1; }
# install.sh must export the leaf file paths, not the whole secret dir.
grep -q 'BROCCOLI_TLS_CERT' "$inst" || { echo "FAIL: install.sh does not export BROCCOLI_TLS_CERT (leaf cert path)"; exit 1; }
grep -q 'BROCCOLI_TLS_KEY'  "$inst" || { echo "FAIL: install.sh does not export BROCCOLI_TLS_KEY (leaf key path)"; exit 1; }
# Caddyfile keeps Caddy-native placeholders (never pre-rendered).
grep -qF 'tls {$TLS_CERT} {$TLS_KEY}' "$cf" || { echo "FAIL: Caddyfile lost its Caddy {\$VAR} tls placeholders"; exit 1; }
# --- M-A: gateway-bypass hardening -------------------------------------------
# Cookies are Secure behind the gateway because it is a trusted proxy that
# forwards the https scheme, not by a setting: Caddy sends X-Forwarded-Proto.
# trusted proxies set (non-empty) so XFF yields real client IPs, not the gateway.
grep -q 'BROCCOLI__SERVER__TRUSTED_PROXIES' "$srv" \
  || { echo "FAIL: server-airgap override does not set TRUSTED_PROXIES"; exit 1; }
# Never a whole private range or loopback: any contestant inside a trusted
# range could forge X-Forwarded-For and dodge the login limits.
grep -E '10\.0\.0\.0/8|192\.168\.0\.0/16|172\.16\.0\.0/12|127\.' "$srv" "$gw" \
  && { echo "FAIL: the airgap trust list must name only the gateway"; exit 1; } || true
# The gateway template stands alone: it declares no service but the gateway,
# so it also runs on a host of its own.
[ "$(grep -cE '^  [a-z][a-z0-9_-]*:$' "$gw")" -ge 1 ] && ! grep -qE '^  server:' "$gw" \
  || { echo "FAIL: the gateway template must not define the server service"; exit 1; }
# install.sh binds plaintext :3000 to host loopback (no LAN bypass around 443).
grep -Eq 'BROCCOLI_HTTP_BIND="\$\{BROCCOLI_HTTP_BIND:-127\.0\.0\.1:3000\}"' "$inst" \
  || { echo "FAIL: install.sh does not loopback-bind BROCCOLI_HTTP_BIND"; exit 1; }

# --- docker-gated: prove the MERGED server binds :3000 to loopback ONLY -------
# Faithfully reproduce the deploy's precedence: install.sh EXPORTS
# BROCCOLI_HTTP_BIND=127.0.0.1:3000 while `docker compose --env-file` carries
# 0.0.0.0:3000 (as .env.server does) — the shell export must win. `compose
# config` renders host_ip on the line ABOVE `target:`, so anchor on the
# server's `target: 3000` and read host_ip back one line; the gateway's
# 443/0.0.0.0 publish is a different target and correctly ignored.
if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
  rel="$here/../.."
  tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
  printf 'BROCCOLI_HTTP_BIND=0.0.0.0:3000\n' > "$tmp/envfile"
  if BROCCOLI_HTTP_BIND=127.0.0.1:3000 \
     BROCCOLI_SERVER_IMAGE=example/broccoli-server:test \
     BROCCOLI__SERVER__ID=1 BROCCOLI__DATABASE__URL=x BROCCOLI__MQ__URL=x \
     BROCCOLI__AUTH__JWT_SECRET=x BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD=x \
     BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT=x \
     BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY=x \
     BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY=x \
     LAN_HOST=contest.lan BROCCOLI_TLS_CERT="$tmp/server.crt" BROCCOLI_TLS_KEY="$tmp/server.key" \
     BROCCOLI_GATEWAY_TRUSTED_PROXIES='["172.31.250.0/28"]' \
       docker compose --env-file "$tmp/envfile" \
                      -f "$rel/docker-compose.server.yaml.template" \
                      -f "$rel/docker-compose.server-airgap.yaml.template" \
                      -f "$rel/docker-compose.gateway-airgap.yaml.template" \
                      config > "$tmp/merged.yaml" 2>/dev/null; then
    # server's published :3000 must bind 127.0.0.1 (export beats --env-file).
    sip="$(grep -B2 'target: 3000' "$tmp/merged.yaml" | grep 'host_ip:' | tr -d '[:space:]' | tail -1)"
    [ "$sip" = "host_ip:127.0.0.1" ] \
      || { echo "FAIL: server :3000 not loopback-bound; shell export must beat --env-file (got '${sip:-none}')"; exit 1; }
    # The gateway is only on the edge network, which the server also joins.
    python3 - "$tmp/merged.yaml" <<'PY' || { echo "FAIL: gateway and server do not share only the edge network"; exit 1; }
import sys, re
text = open(sys.argv[1]).read()
def nets(svc):
    block = re.search(r"^  %s:\n((?:    .*\n|\n)*)" % svc, text, re.M).group(1)
    m = re.search(r"^    networks:\n((?:      .*\n)*)", block, re.M)
    return set(re.findall(r"^      ([a-z_-]+):", m.group(1), re.M))
assert nets("gateway") == {"edge"}, nets("gateway")
assert nets("server") == {"default", "edge"}, nets("server")
PY
    # Standalone gateway host: the gateway template alone renders.
    LAN_HOST=contest.lan BROCCOLI_TLS_CERT="$tmp/server.crt" BROCCOLI_TLS_KEY="$tmp/server.key" \
    BROCCOLI_UPSTREAMS='10.0.0.21:3000 10.0.0.22:3000' \
      docker compose -f "$rel/docker-compose.gateway-airgap.yaml.template" config -q \
      || { echo "FAIL: gateway-airgap template does not run on a host of its own"; exit 1; }
    echo "PASS: merged server publishes :3000 on 127.0.0.1 (export beats --env-file)"
  else
    # docker is present and every interpolation var is supplied, so a config
    # failure means OUR override is broken — fail loudly, never mask it as SKIP.
    echo "FAIL: docker present but 'compose config' failed on the airgap override"; exit 1
  fi
else
  echo "SKIP: docker unavailable — merge proof skipped (deterministic checks ran)"
fi

echo "PASS: TLS gateway wired; no envsubst; Caddy-native placeholders intact; gateway is sole LAN ingress"
