#!/usr/bin/env bash
# release/install.sh dry runs: values passed at install time reach every env
# file, each role gets its own Compose project, and BROCCOLI_PLUGINS enables
# optional plugins.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
cp -r "$here/../." "$T/"
cd "$T"
mkdir -p plugins plugins-available/codelink-bracket
printf 'name = "codelink-bracket"\n' > plugins-available/codelink-bracket/plugin.toml

fail() { echo "FAIL: $*"; exit 1; }
# shellcheck source=../lib/env.sh
. ./lib/env.sh
run() { BROCCOLI_DRY_RUN=1 BROCCOLI_SKIP_TIME_CHECK=1 ./install.sh "$@" </dev/null >/dev/null 2>&1; }

BROCCOLI__AUTH__SECURE_COOKIES=true \
BROCCOLI__STORAGE__OBJECT_STORAGE__REGION=cn-north-1 \
BROCCOLI__STORAGE__OBJECT_STORAGE__PATH_STYLE=false \
  run infra || fail "infra dry run"
for f in .env.infra server-secrets.env; do
  grep -qx "BROCCOLI__AUTH__SECURE_COOKIES='true'" "$f" || fail "$f ignored SECURE_COOKIES"
done
for f in .env.infra connection.env; do
  grep -qx "BROCCOLI__STORAGE__OBJECT_STORAGE__REGION='cn-north-1'" "$f" || fail "$f ignored REGION"
  grep -qx "BROCCOLI__STORAGE__OBJECT_STORAGE__PATH_STYLE='false'" "$f" || fail "$f ignored PATH_STYLE"
done
grep -qx "COMPOSE_PROJECT_NAME='broccoli-infra'" .env.infra || fail "infra has no project name"

BROCCOLI_PLUGINS="codelink-bracket" run server || fail "server dry run"
grep -qx "COMPOSE_PROJECT_NAME='broccoli-server'" .env.server || fail "server has no project name"
grep -qx "BROCCOLI__AUTH__SECURE_COOKIES='true'" .env.server || fail "server lost SECURE_COOKIES"
[ -f plugins/codelink-bracket/plugin.toml ] || fail "BROCCOLI_PLUGINS did not enable codelink-bracket"

# Re-running a role after infra moved (rehearsal -> venue) keeps the env file
# but takes the infra node's new connection settings.
env_set connection.env BROCCOLI__DATABASE__URL 'postgres://postgres:pw@10.9.9.9:5432/broccoli'
env_set server-secrets.env BROCCOLI__AUTH__JWT_SECRET 'rotated-jwt-secret-rotated-jwt-secret'
server_id="$(env_get .env.server BROCCOLI__SERVER__ID)"
run server || fail "server re-run"
[ "$(env_get .env.server BROCCOLI__DATABASE__URL)" = 'postgres://postgres:pw@10.9.9.9:5432/broccoli' ] \
  || fail "server re-run kept the old database URL"
[ "$(env_get .env.server BROCCOLI__AUTH__JWT_SECRET)" = 'rotated-jwt-secret-rotated-jwt-secret' ] \
  || fail "server re-run kept the old JWT secret"
[ "$(env_get .env.server BROCCOLI__SERVER__ID)" = "$server_id" ] || fail "server re-run changed its own id"

# Behind a gateway, a server trusts exactly the gateway for X-Forwarded-For,
# and the gateway install says what to pass.
BROCCOLI_GATEWAY_HOSTS='10.0.0.5 10.0.0.6' run server || fail "server run with gateway hosts"
[ "$(env_get .env.server BROCCOLI__SERVER__TRUSTED_PROXIES)" = '["10.0.0.5/32","10.0.0.6/32"]' ] \
  || fail "server does not trust exactly the gateways"
if BROCCOLI_GATEWAY_HOSTS='10.0.0.5;rm' run server; then fail "a malformed gateway address was accepted"; fi
BROCCOLI_UPSTREAMS='10.0.0.21:3000' BROCCOLI_DRY_RUN=1 BROCCOLI_SKIP_TIME_CHECK=1 \
  ./install.sh gateway </dev/null >"$T/gateway.log" 2>&1 || fail "gateway dry run"
grep -q "BROCCOLI_GATEWAY_HOSTS='.*' ./install.sh server" "$T/gateway.log" \
  || fail "gateway install does not say how to trust it on the servers"

rm -f .env.worker
if BROCCOLI_PLUGINS="no-such-plugin" run worker; then
  fail "an unknown plugin in BROCCOLI_PLUGINS was accepted"
fi

# Operator passwords survive as themselves: single-quoted in env files (no
# Compose interpolation, no ' #' comment) and percent-encoded inside URLs.
mkdir quoting && cp -r install.sh lib docker-compose.*.template quoting/ && cd quoting
POSTGRES_PASSWORD='p@ss #1/$x?' REDIS_PASSWORD='r%d' BROCCOLI_ADMIN_PASSWORD='Adm$n #1' \
  BROCCOLI_INFRA_HOST=10.0.0.10 run infra || fail "infra dry run with punctuated passwords"
[ "$(env_get .env.infra POSTGRES_PASSWORD)" = 'p@ss #1/$x?' ] || fail "POSTGRES_PASSWORD not kept verbatim"
[ "$(env_get server-secrets.env BROCCOLI_BOOTSTRAP_ADMIN_PASSWORD)" = 'Adm$n #1' ] || fail "admin password not kept verbatim"
[ "$(env_get connection.env BROCCOLI__DATABASE__URL)" = 'postgres://postgres:p%40ss%20%231%2F%24x%3F@10.0.0.10:5432/broccoli' ] \
  || fail "database URL password not percent-encoded"
[ "$(env_get connection.env BROCCOLI__MQ__URL)" = 'redis://:r%25d@10.0.0.10:6379' ] || fail "redis URL password not percent-encoded"
cd ..

# A value no env-file reader agrees on is refused, and no env file is left.
mkdir refused && cp -r install.sh lib docker-compose.*.template refused/ && cd refused
if BROCCOLI_ADMIN_PASSWORD="it's" run infra; then fail "a single quote in a value was accepted"; fi
[ ! -e .env.infra ] && [ ! -e .env.infra.tmp ] || fail "a refused value left a partial .env.infra"
if BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY='a"b' run infra; then fail "a quote in an S3 key was accepted"; fi
cd ..

# An env file copied from an example still holds change-me placeholders.
cp .env.server.example refused/.env.server
cd refused
if run server; then fail "a server env file full of change-me placeholders was accepted"; fi
cd ..

# The infra address is never guessed silently: no address stops an infra
# install, several make it name the one it picked.
mkdir -p fakebin addr && cp -r install.sh lib docker-compose.*.template addr/
printf '#!/bin/sh\n[ "$1" = -I ] && printf "%%s\\n" "$FAKE_ADDRS"\n' > fakebin/hostname
chmod +x fakebin/hostname
cd addr
if FAKE_ADDRS="" PATH="$T/fakebin:$PATH" run infra; then fail "infra install guessed 127.0.0.1"; fi
FAKE_ADDRS="192.168.1.5 10.0.0.7" PATH="$T/fakebin:$PATH" BROCCOLI_DRY_RUN=1 BROCCOLI_SKIP_TIME_CHECK=1 \
  ./install.sh infra </dev/null >"$T/addr.log" 2>&1 || fail "infra dry run with two addresses"
grep -q 'several addresses (192.168.1.5 10.0.0.7)' "$T/addr.log" || fail "no warning about the guessed infra address"
[ "$(env_get connection.env BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT)" = 'http://192.168.1.5:8333' ] \
  || fail "infra address not taken from the first address"
cd ..
echo PASS
