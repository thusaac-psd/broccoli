#!/usr/bin/env bash
# A server configured as the runbook says for PgBouncer boots through one and
# loads a plugin that uses SQL: PgBouncer 1.26 in session mode, auth_query over
# release/pgbouncer/user-lookup.sql, extra_float_digits ignored, and the
# server's database.statement_timeout_secs = 0 and tcp_keepalives = false.
# It runs a server binary (BROCCOLI_SERVER_BIN, default target/debug/server,
# built with `cargo build -p server`) against Postgres, Redis and PgBouncer
# containers, and skips when the binary or an image is not here, since it
# never pulls one.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
server="${BROCCOLI_SERVER_BIN:-$root/target/debug/server}"
pgbouncer_image="${PGBOUNCER_IMAGE:-mirror.gcr.io/edoburu/pgbouncer:v1.26.1-p0}"
images=(postgres:17-alpine redis:7-alpine "$pgbouncer_image")

fail() {
  echo "FAIL: $*"
  exit 1
}
skip() {
  echo "SKIP: $*"
  exit 0
}

[ -x "$server" ] || skip "no server binary at $server"
docker info >/dev/null 2>&1 || skip "no docker engine"
for image in "${images[@]}"; do
  docker image inspect "$image" >/dev/null 2>&1 || skip "image $image is not here"
done

T="$(mktemp -d)"
net="broccoli-pgb-$$"
pg="$net-pg"
redis="$net-redis"
bouncer="$net-bouncer"
pid=""
cleanup() {
  [ -n "$pid" ] && kill "$pid" 2>/dev/null && wait "$pid" 2>/dev/null
  docker rm -f "$pg" "$redis" "$bouncer" >/dev/null 2>&1 || true
  docker network rm "$net" >/dev/null 2>&1 || true
  rm -rf "$T"
}
trap cleanup EXIT

# free_port: a TCP port nothing listens on.
free_port() {
  python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'
}
# wait_for SECONDS COMMAND...: until COMMAND succeeds, or fail.
wait_for() {
  local deadline=$((SECONDS + $1))
  shift
  until "$@" >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 1
  done
}

docker network create "$net" >/dev/null
docker run -d --name "$pg" --network "$net" \
  -e POSTGRES_PASSWORD=superuser_secret -e POSTGRES_DB=broccoli \
  -e BROCCOLI_DB_PASSWORD=app_secret \
  -v "$root/config/postgres-init/20-broccoli-app-role.sh:/docker-entrypoint-initdb.d/20-broccoli-app-role.sh:ro" \
  postgres:17-alpine >/dev/null
redis_port="$(free_port)"
docker run -d --name "$redis" --network "$net" -p "127.0.0.1:$redis_port:6379" redis:7-alpine >/dev/null
# The init script runs on a server that listens only on its socket; wait for
# the one that takes TCP connections.
wait_for 60 docker exec "$pg" psql -h 127.0.0.1 -U postgres -d broccoli -c 'SELECT 1' \
  || fail "postgres did not start"
docker exec -i "$pg" psql -v ON_ERROR_STOP=1 -v app=broccoli -U postgres -d broccoli -q \
  <"$root/release/pgbouncer/user-lookup.sql" || fail "user-lookup.sql did not apply"

bouncer_port="$(free_port)"
docker run -d --name "$bouncer" --network "$net" -p "127.0.0.1:$bouncer_port:5432" \
  -e DB_HOST="$pg" -e DB_NAME=broccoli -e DB_USER=broccoli -e DB_PASSWORD=app_secret \
  -e POOL_MODE=session -e AUTH_TYPE=scram-sha-256 \
  -e 'AUTH_QUERY=SELECT uname, phash FROM pgbouncer.user_lookup($1)' \
  -e IGNORE_STARTUP_PARAMETERS=extra_float_digits \
  -e DEFAULT_POOL_SIZE=100 -e MAX_CLIENT_CONN=500 \
  "$pgbouncer_image" >/dev/null

mkdir -p "$T/plugins"
cp -r "$root/plugins/print" "$T/plugins/"
[ -f "$T/plugins/print/print_plugin.wasm" ] || skip "the print plugin's wasm is not built"
http_port="$(free_port)"
cat >"$T/config.toml" <<TOML
[database]
url = "postgres://broccoli:app_secret@127.0.0.1:$bouncer_port/broccoli"
statement_timeout_secs = 0
tcp_keepalives = false
[mq]
url = "redis://127.0.0.1:$redis_port"
[auth]
jwt_secret = "pgbouncer-boot-test-secret-0123456789abcdef"
[bootstrap]
admin_username = "admin"
admin_password = "pgbouncer-boot-test"
[storage]
backend = "database"
[plugin]
plugins_dir = "$T/plugins"
[server]
port = $http_port
id = "pgbouncer-boot-test"
TOML

(cd "$T" && BROCCOLI_CONFIG="$T/config.toml" exec "$server") >"$T/server.log" 2>&1 &
pid=$!

healthy() {
  local body
  body="$(curl -fsS "http://127.0.0.1:$http_port/healthz")" || return 1
  [[ $body == *'"db":"ok"'* && $body == *'"plugins":"ok"'* ]]
}
if ! wait_for 120 healthy; then
  kill -0 "$pid" 2>/dev/null || echo "the server exited"
  tail -n 40 "$T/server.log"
  fail "the server did not come up healthy behind PgBouncer"
fi
grep -q '"Opened the plugin'"'"'s SQL pool","plugin_id":"print"' "$T/server.log" \
  || fail "the print plugin opened no SQL pool through PgBouncer"
if grep -q 'unsupported startup parameter' "$T/server.log"; then
  fail "a session sent a startup parameter PgBouncer refuses"
fi
echo "PASS: a server booted behind PgBouncer and loaded a plugin that uses SQL"
