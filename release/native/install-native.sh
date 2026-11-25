#!/usr/bin/env bash
# Native (non-Docker) installer for an offline Broccoli contest LAN on
# Ubuntu 22.04 (x86_64). Mirrors the battery-verified staging deployment:
# native Postgres + Redis + SeaweedFS + systemd-managed broccoli services,
# isolate sandbox with cgroup-v2 delegation. No Docker, no internet required:
# PostgreSQL, Redis and the toolchain install from the bundle's staged .debs.
#
# Roles:
#   infra-server   box 1: Postgres + Redis + SeaweedFS + broccoli-server.
#                  Generates secrets on the first run and keeps them on re-runs
#                  (--rotate-secrets for new ones); writes connection.env for workers.
#   worker         box 2..N: isolate + compilers + broccoli-worker, pointed at
#                  the infra box over the LAN (reads connection.env).
#
# Usage:
#   sudo BROCCOLI_INFRA_HOST=<box1-LAN-IP> ./install-native.sh infra-server
#   # copy the generated connection.env to each worker box's bundle dir, then:
#   sudo ./install-native.sh worker   # worker id defaults to one unique to this box
set -euo pipefail

BUNDLE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROLE="${1:-}"
DATA_DIR="${BROCCOLI_DATA_DIR:-/data/broccoli}"
SVC_USER="${BROCCOLI_SVC_USER:-$(logname 2>/dev/null || echo ubuntu)}"
DB_NAME="broccoli"; DB_USER="broccoli"
SEAWEED_DATA="${BROCCOLI_SEAWEED_DATA:-/data/seaweed-data}"
CONN_ENV="$BUNDLE/connection.env"
# The helpers every release installer shares sit in lib/ in a bundle and in
# release/lib/ in a repo checkout.
release_lib="$BUNDLE/lib"
[ -f "$release_lib/plugins.sh" ] || release_lib="$BUNDLE/../lib"
for f in plugins env host; do
  # shellcheck source=/dev/null
  . "$release_lib/$f.sh"
done

die(){ echo "error: $*" >&2; exit 1; }
need_root(){ [ "$(id -u)" = 0 ] || die "run with sudo"; }
gen(){ local s; s="$(head -c "${1:-24}" /dev/urandom | base64 | tr -dc 'A-Za-z0-9')"; printf '%s' "${s:0:${2:-32}}"; }
# toml_string FILE KEY: the first `KEY = "value"` in FILE.
toml_string(){ sed -n "s/^$2 *= *\"\(.*\)\"\$/\1/p" "$1" | sed -n 1p; }
# toml_url_password FILE SCHEME: the password in the first SCHEME:// url.
toml_url_password(){ sed -n "s|^url *= *\"$2://[^:]*:\([^@]*\)@.*|\1|p" "$1" | sed -n 1p; }

# (Re)start a role's units after their config and unit files are written. A
# re-run (upgrade, --rotate-secrets, a new LAN) must restart what is already
# running: `systemctl enable --now` leaves an active unit as it is. An old weed
# would then keep its old flags, including the LAN-wide bind that still holds
# :8333, so the S3 socket could not bind and the install would stop before the
# server unit and connection.env were written; and weed, the server and the
# worker would keep their old config and credentials.
start_infra_units(){
  systemctl daemon-reload
  # Free :8333 for the socket first; on a first install there is nothing to stop.
  systemctl stop broccoli-weed-s3.socket broccoli-weed-s3.service broccoli-weed 2>/dev/null || true
  systemctl enable broccoli-weed broccoli-weed-s3.socket broccoli-server
  systemctl restart broccoli-weed
  systemctl restart broccoli-weed-s3.socket
  sleep 3
  systemctl restart broccoli-server
}

start_worker_units(){
  systemctl daemon-reload
  systemctl enable broccoli-isolate-setup broccoli-worker
  systemctl restart broccoli-isolate-setup
  systemctl restart broccoli-worker
}

# weed_drop_unknown_flags UNIT: the bundle pins the SeaweedFS release the unit
# was written for, but a bundle built with another one may not know
# -s3.port.iceberg, and weed refuses to start on an unknown flag. A weed that
# lacks the flag opens no Iceberg port either, so drop it there.
weed_drop_unknown_flags(){
  local help
  # `weed server -h` exits nonzero, so read its text before matching.
  help="$("$BUNDLE/bin/weed" server -h 2>&1 || true)"
  case "$help" in *-s3.port.iceberg*) return 0 ;; esac
  sed -i 's/ -s3\.port\.iceberg=0//' "$1"
  echo ">> this weed has no -s3.port.iceberg (and no Iceberg port); dropped the flag"
}

require_ubuntu(){
  . /etc/os-release 2>/dev/null || true
  [ "${ID:-}" = ubuntu ] || echo "warning: not Ubuntu (ID=${ID:-?}); proceeding anyway" >&2
  [ "$(uname -m)" = x86_64 ] || die "bundle binaries are x86_64; this host is $(uname -m)"
}

# The Ubuntu version whose bundled .deb sets this box installs.
ubuntu_version(){ (. /etc/os-release 2>/dev/null; echo "${VERSION_ID:-}"); }

# first_match PATH...: print the first of PATH (globs the caller expanded)
# that exists, or nothing. Globbing and no pipeline, so a missing file is an
# empty answer the caller reports, never a failure `set -euo pipefail` stops
# the install on without saying why.
first_match(){
  local path
  for path in "$@"; do
    [ -e "$path" ] && { printf '%s\n' "$path"; return 0; }
  done
  return 0
}

# install_bundled_set NAME PACKAGE: install the bundle's NAME/<VERSION_ID>/*.deb
# set (stage-NAME.sh) with dpkg, offline. Returns 1 when the bundle has no set
# for this Ubuntu version. A set that does not install, or leaves PACKAGE
# anything but installed, stops the install with dpkg's own output.
install_bundled_set(){
  local name="$1" package="$2" ver debs log status
  ver="$(ubuntu_version)"
  debs="$BUNDLE/$name/$ver"
  [ -n "$(first_match "$debs"/*.deb)" ] || return 1
  echo ">> installing the bundled $name set for Ubuntu $ver ($debs)"
  log="$(mktemp)"
  if ! dpkg -i --skip-same-version "$debs"/*.deb >"$log" 2>&1; then
    cat "$log" >&2
    rm -f "$log"
    die "dpkg could not install the bundled $name set in $debs (output above)"
  fi
  rm -f "$log"
  status="$(dpkg-query -W -f='${Status}' "$package" 2>/dev/null || true)"
  [ "$status" = "install ok installed" ] \
    || die "dpkg left $package as '${status:-not installed}' after installing $debs"
}

# die_unstaged NAME WHAT: stop because the bundle has no NAME set for this
# Ubuntu version and the box does not already have WHAT. The install never
# reaches for the network: a closed lab has only the bundle.
die_unstaged(){
  local ver
  ver="$(ubuntu_version)"
  die "$2 is not installed and this bundle has no $1 set for Ubuntu ${ver:-?}.
  Stage it on a box with internet: ./stage-$1.sh ${ver:-<version>}, rebuild the bundle, and re-run.
  Or install $2 yourself first."
}

# The message queue needs Redis 7 or newer (stream commands from 6.2 and 7.0);
# Ubuntu 22.04 ships 6.0, so the bundle carries the set (stage-redis.sh). A
# box that already runs Redis 7 keeps it.
redis_major(){ redis-server --version 2>/dev/null | sed -n 's/.* v=\([0-9]*\)\..*/\1/p'; }
ensure_redis7(){
  [ "$(redis_major)" -ge 7 ] 2>/dev/null && return 0
  install_bundled_set redis redis-server || die_unstaged redis "Redis 7 or newer"
  [ "$(redis_major)" -ge 7 ] 2>/dev/null \
    || die "the bundled redis set installed Redis $(redis_major), not 7 or newer; restage it with ./stage-redis.sh"
}

# PostgreSQL, from the bundled set for this Ubuntu version (stage-postgres.sh:
# the major version the release's compose files run). A box that already runs
# a PostgreSQL server keeps it.
ensure_postgres(){
  local set major
  grep -q '^postgresql-[0-9]* install ok installed$' \
    <<<"$(dpkg-query -W -f='${Package} ${Status}\n' 'postgresql-[0-9]*' 2>/dev/null)" && return 0
  set="$(first_match "$BUNDLE/postgres/$(ubuntu_version)"/postgresql-[0-9]*_*.deb)"
  [ -n "$set" ] || die_unstaged postgres "a PostgreSQL server"
  major="$(basename "$set" | sed -n 's/^postgresql-\([0-9]*\)_.*/\1/p')"
  install_bundled_set postgres "postgresql-$major"
}

# The worker's C/C++/Python toolchain, from the bundled set for this Ubuntu
# version (stage-toolchain.sh) unless the box already has one. Then VERIFY
# with a real compile: a worker that cannot compile C/C++ would wrongly CE
# every such submission, so the install refuses to bring one up. Deliberate
# Python-only pool? set BROCCOLI_ALLOW_NO_CXX=1.
ensure_toolchain(){
  local ver rc=0 t
  ver="$(ubuntu_version)"
  if ! dpkg -s build-essential >/dev/null 2>&1; then
    # --skip-same-version (install_bundled_set): the set is the full closure,
    # so a live Desktop's base libs are skipped and only the missing
    # toolchain packages install.
    install_bundled_set toolchain build-essential \
      || [ "${BROCCOLI_ALLOW_NO_CXX:-0}" = 1 ] \
      || die_unstaged toolchain "a C/C++ toolchain (build-essential)"
  fi
  t="$(mktemp -d)"
  printf 'int main(){return 0;}\n' > "$t/a.c"
  printf 'int main(){return 0;}\n' > "$t/a.cpp"
  { command -v gcc >/dev/null 2>&1 && gcc "$t/a.c"   -o "$t/a"  2>/dev/null; } || rc=1
  { command -v g++ >/dev/null 2>&1 && g++ "$t/a.cpp" -o "$t/ax" 2>/dev/null; } || rc=1
  command -v python3 >/dev/null 2>&1 || rc=1
  rm -rf "$t"
  if [ "$rc" != 0 ]; then
    if [ "${BROCCOLI_ALLOW_NO_CXX:-0}" = 1 ]; then
      echo "warning: C/C++ toolchain not working; proceeding Python-only (BROCCOLI_ALLOW_NO_CXX=1)" >&2
      return 0
    fi
    die "C/C++ toolchain missing or broken on this worker (Ubuntu ${ver:-?}).
  A worker without working gcc/g++ would wrongly CE every C/C++ submission, so install refuses to continue.
  Stage the toolchain on a box with internet: ./stage-toolchain.sh ${ver:-<version>}, rebuild the bundle,
  and re-run: sudo ./install-native.sh worker
  (Deliberate Python-only worker pool? re-run with BROCCOLI_ALLOW_NO_CXX=1.)"
  fi
  echo ">> toolchain OK: $(gcc --version | sed -n 1p) / $(g++ --version | sed -n 1p) / $(python3 --version 2>&1)"
}

install_common_dirs(){
  install -d -o "$SVC_USER" -g "$SVC_USER" "$DATA_DIR" "$DATA_DIR/target/release" \
    "$DATA_DIR/config" "$DATA_DIR/logs" "$DATA_DIR/tools" "$DATA_DIR/scripts" "${DATA_DIR}-data"
}

# ---------------------------------------------------------------- infra-server
install_infra_server(){
  local host="${BROCCOLI_INFRA_HOST:-}"
  [ -n "$host" ] || die "set BROCCOLI_INFRA_HOST=<this box's LAN IP> for infra-server"
  require_ubuntu; install_common_dirs
  ensure_postgres
  ensure_redis7

  # binaries
  install -m755 "$BUNDLE/bin/server" "$DATA_DIR/target/release/server"
  install -m755 "$BUNDLE/bin/broccoli-compare" "$DATA_DIR/tools/broccoli-compare"
  install -m755 "$BUNDLE/bin/weed" /usr/local/bin/weed
  # plugins + web frontend. Optional plugins named in BROCCOLI_PLUGINS (or
  # enabled earlier with ./enable-plugin.sh) are in the bundle's plugins/.
  enable_requested_plugins "$BUNDLE"
  rm -rf "$DATA_DIR/plugins"; cp -a "$BUNDLE/plugins" "$DATA_DIR/plugins"
  install -d "$DATA_DIR/packages/web/build"; rm -rf "$DATA_DIR/packages/web/build/client"
  cp -a "$BUNDLE/web/client" "$DATA_DIR/packages/web/build/client"
  chown -R "$SVC_USER":"$SVC_USER" "$DATA_DIR/plugins" "$DATA_DIR/packages" "$DATA_DIR/tools"

  # secrets: a re-run (to update binaries, say) keeps the ones config.toml
  # already holds, since new ones would lock every worker out of the DB, MQ and
  # S3 mid-event and log every user out. --rotate-secrets makes new ones.
  local db_pw s3_ak s3_sk jwt admin_pw redis_pw conf="$DATA_DIR/config/config.toml"
  if [ "$ROTATE_SECRETS" = 0 ] && [ -f "$conf" ]; then
    echo ">> reusing the secrets in $conf (pass --rotate-secrets for new ones)"
    db_pw="$(toml_url_password "$conf" postgres)"; redis_pw="$(toml_url_password "$conf" redis)"
    s3_ak="$(toml_string "$conf" access_key)"; s3_sk="$(toml_string "$conf" secret_key)"
    jwt="$(toml_string "$conf" jwt_secret)"; admin_pw="$(toml_string "$conf" admin_password)"
  fi
  [ -n "${db_pw:-}" ] || db_pw="$(gen)"
  [ -n "${redis_pw:-}" ] || redis_pw="$(gen)"
  [ -n "${s3_ak:-}" ] || s3_ak="$(gen 16 18)"
  [ -n "${s3_sk:-}" ] || s3_sk="$(gen 24 32)"
  [ -n "${jwt:-}" ] || jwt="$(gen 48 64)"
  admin_pw="${BROCCOLI_ADMIN_PASSWORD:-${admin_pw:-$(gen)}}"

  # postgres: role + db + LAN access (scram)
  echo ">> configuring postgres"
  grep -q 1 <<<"$(sudo -u postgres psql -tc "SELECT 1 FROM pg_roles WHERE rolname='$DB_USER'")" \
    || sudo -u postgres psql -c "CREATE ROLE $DB_USER LOGIN CREATEROLE PASSWORD '$db_pw'"
  # CREATEROLE and nothing more: the server provisions each plugin's own
  # role and schema with it.
  sudo -u postgres psql -c "ALTER ROLE $DB_USER LOGIN CREATEROLE PASSWORD '$db_pw'"
  grep -q 1 <<<"$(sudo -u postgres psql -tc "SELECT 1 FROM pg_database WHERE datname='$DB_NAME'")" \
    || sudo -u postgres createdb -O "$DB_USER" "$DB_NAME"
  local pgconf pghba subnet
  pgconf="$(first_match /etc/postgresql/*/main/postgresql.conf)"
  [ -n "$pgconf" ] || die "no /etc/postgresql/<major>/main/postgresql.conf: the PostgreSQL server package did not create its cluster"
  pghba="$(dirname "$pgconf")/pg_hba.conf"
  subnet="${BROCCOLI_LAN_CIDR:-${host%.*}.0/24}"
  grep -q "^listen_addresses *= *'\*'" "$pgconf" || sed -i "s/^#\?listen_addresses.*/listen_addresses = '*'/" "$pgconf"
  # The stock 100 connections fit one server and a handful of workers; 500
  # fits this server and a lab of workers (docs/operator-runbook.md). A
  # higher value an operator set stays.
  [ "$(sudo -u postgres psql -tAc "SHOW max_connections")" -ge 500 ] \
    || sudo -u postgres psql -c "ALTER SYSTEM SET max_connections = 500"
  # Replace the rule on every run, so a new LAN (rehearsal -> venue) is let in.
  sed -i '/# broccoli-lan$/d' "$pghba"
  echo "host $DB_NAME $DB_USER $subnet scram-sha-256 # broccoli-lan" >> "$pghba"
  # The plugin roles the server provisions are members of the database's
  # plugin group role, named by the server itself.
  local plugin_group
  plugin_group="$("$BUNDLE/bin/server" --plugin-group-role "$DB_NAME")"
  echo "host $DB_NAME +$plugin_group $subnet scram-sha-256 # broccoli-lan" >> "$pghba"
  systemctl restart postgresql

  # redis: LAN bind + auth
  echo ">> configuring redis"
  local rconf=/etc/redis/redis.conf
  sed -i "s/^bind .*/bind 127.0.0.1 $host/" "$rconf"
  sed -i "s/^# *requirepass .*/requirepass $redis_pw/; s/^requirepass .*/requirepass $redis_pw/" "$rconf"
  grep -q "^requirepass " "$rconf" || echo "requirepass $redis_pw" >> "$rconf"
  # Queue entries are correctness-bearing: never evict them, and keep a crash
  # to the last second of writes.
  local opt
  for opt in "appendonly yes" "appendfsync everysec" "maxmemory-policy noeviction"; do
    sed -i "s/^# *${opt%% *} .*/$opt/; s/^${opt%% *} .*/$opt/" "$rconf"
    grep -q "^${opt%% *} " "$rconf" || echo "$opt" >> "$rconf"
  done
  systemctl restart redis-server

  # seaweedfs s3 identity + service. SeaweedFS itself listens on loopback
  # only; workers reach just its credentialed S3 API, forwarded from
  # $host:8333 by a systemd socket (the filer would otherwise hand any
  # contestant on the LAN every testcase without credentials).
  echo ">> configuring seaweedfs"
  local proxyd
  proxyd="$(first_match /usr/lib/systemd/systemd-socket-proxyd /lib/systemd/systemd-socket-proxyd)"
  [ -n "$proxyd" ] || die "systemd-socket-proxyd not found (it ships with systemd); needed to expose only S3 on the LAN"
  install -d -o "$SVC_USER" -g "$SVC_USER" "$SEAWEED_DATA"
  sed -e "s/@ACCESS_KEY@/$s3_ak/; s/@SECRET_KEY@/$s3_sk/" \
    "$BUNDLE/config/seaweedfs-s3.json.tmpl" > "$DATA_DIR/config/seaweedfs-s3.json"
  for unit in broccoli-weed.service broccoli-weed-s3.socket broccoli-weed-s3.service; do
    sed -e "s|@WEED_IP@|$host|; s|@SEAWEED_DATA@|$SEAWEED_DATA|; s|@DATA_DIR@|$DATA_DIR|; s|@USER@|$SVC_USER|; s|@SOCKET_PROXYD@|$proxyd|" \
      "$BUNDLE/systemd/$unit.tmpl" > "/etc/systemd/system/$unit"
  done
  weed_drop_unknown_flags /etc/systemd/system/broccoli-weed.service

  # config.toml
  sed -e "s|@DB_URL@|postgres://$DB_USER:$db_pw@$host:5432/$DB_NAME|" \
      -e "s|@MQ_URL@|redis://:$redis_pw@$host:6379|" \
      -e "s|@S3_ENDPOINT@|http://$host:8333|" \
      -e "s|@S3_ACCESS_KEY@|$s3_ak|; s|@S3_SECRET_KEY@|$s3_sk|" \
      -e "s|@JWT_SECRET@|$jwt|; s|@ADMIN_PASSWORD@|$admin_pw|" \
      -e "s|@DATA_DIR@|$DATA_DIR|; s|@SERVER_ID@|server-1|; s|@WORKER_ID@|server-1|" \
      -e "s|@HTTP_HOST@|0.0.0.0|; s|@HTTP_PORT@|80|; s|@INFRA_HOST@|$host|" \
      "$BUNDLE/config/config.toml.tmpl" > "$DATA_DIR/config/config.toml"
  chown "$SVC_USER":"$SVC_USER" "$DATA_DIR/config/config.toml" "$DATA_DIR/config/seaweedfs-s3.json"
  chmod 600 "$DATA_DIR/config/config.toml"

  # server unit
  sed -e "s|@DATA_DIR@|$DATA_DIR|; s|@USER@|$SVC_USER|" \
    "$BUNDLE/systemd/broccoli-server.service.tmpl" > /etc/systemd/system/broccoli-server.service

  start_infra_units

  # connection.env for workers (S3 endpoint + DB/MQ on the LAN)
  umask 077
  cat > "$CONN_ENV" <<EOF
# Generated by install-native.sh infra-server on $host. Copy to each worker box.
BROCCOLI_INFRA_HOST=$host
BROCCOLI__DATABASE__URL=postgres://$DB_USER:$db_pw@$host:5432/$DB_NAME
BROCCOLI__MQ__URL=redis://:$redis_pw@$host:6379
BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT=http://$host:8333
BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY=$s3_ak
BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY=$s3_sk
BROCCOLI__STORAGE__OBJECT_STORAGE__BUCKET=broccoli-blobs
EOF
  echo
  echo ">> infra-server up on $host. admin password: $admin_pw"
  echo ">> connection.env written to $CONN_ENV -- copy it to every worker box's bundle dir."
  echo ">> import problems with:  cd contest && sudo ./import-problems.sh <archive>"
}

# ---------------------------------------------------------------------- worker
install_worker(){
  [ -f "$CONN_ENV" ] || die "connection.env not found in $BUNDLE (copy it from the infra box)"
  # shellcheck disable=SC1090
  set -a; . "$CONN_ENV"; set +a
  # Native workers always read blobs from the infra box's SeaweedFS.
  BROCCOLI__STORAGE__BACKEND=object_storage env_require_role worker \
    || die "fix $CONN_ENV (copy the one install-native.sh infra-server wrote)"
  # Unset, the worker derives its id at boot (common::node_identity).
  local wid="${BROCCOLI__WORKER__ID:-}"
  require_ubuntu; install_common_dirs
  ensure_toolchain

  # isolate (setuid) + cgroup-v2 delegation
  echo ">> installing isolate"
  install -m4755 -o root -g root "$BUNDLE/bin/isolate" /usr/local/bin/isolate
  install -m644 "$BUNDLE/config/isolate.conf" /usr/local/etc/isolate
  install -m755 "$BUNDLE/scripts/fix-live-ubuntu-isolate.sh" "$DATA_DIR/scripts/fix-live-ubuntu-isolate.sh"
  cp "$BUNDLE/systemd/broccoli-isolate-setup.service" /etc/systemd/system/broccoli-isolate-setup.service
  sed -i "s|@DATA_DIR@|$DATA_DIR|g" /etc/systemd/system/broccoli-isolate-setup.service 2>/dev/null || true

  # binaries
  install -m755 "$BUNDLE/bin/worker" "$DATA_DIR/target/release/worker"
  install -m755 "$BUNDLE/bin/broccoli-compare" "$DATA_DIR/tools/broccoli-compare"
  chown -R "$SVC_USER":"$SVC_USER" "$DATA_DIR/tools"

  # Worker config goes to its OWN file (config/worker.toml), NOT config.toml.
  # The worker binary reads $BROCCOLI_CONFIG (set in its unit). This is critical:
  # on an all-in-one box (infra-server + worker together) writing config.toml
  # would clobber the server's [server] host/port (-> 127.0.0.1:8080, which
  # collides with SeaweedFS on 8080) and the breakage only surfaces on the next
  # reboot. A separate file lets both coexist and survive reboots.
  sed -e "s|@DB_URL@|$BROCCOLI__DATABASE__URL|" \
      -e "s|@MQ_URL@|$BROCCOLI__MQ__URL|" \
      -e "s|@S3_ENDPOINT@|$BROCCOLI__STORAGE__OBJECT_STORAGE__ENDPOINT|" \
      -e "s|@S3_ACCESS_KEY@|$BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY|" \
      -e "s|@S3_SECRET_KEY@|$BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY|" \
      -e "s|@JWT_SECRET@|unused-on-worker|; s|@ADMIN_PASSWORD@|unused|" \
      -e "s|@DATA_DIR@|$DATA_DIR|; s|@SERVER_ID@|$wid|; s|@WORKER_ID@|$wid|" \
      -e "s|@HTTP_HOST@|127.0.0.1|; s|@HTTP_PORT@|8081|; s|@INFRA_HOST@|${BROCCOLI_INFRA_HOST:-127.0.0.1}|" \
      "$BUNDLE/config/config.toml.tmpl" > "$DATA_DIR/config/worker.toml"
  chown "$SVC_USER":"$SVC_USER" "$DATA_DIR/config/worker.toml"; chmod 600 "$DATA_DIR/config/worker.toml"

  # worker unit + isolate drop-in
  sed -e "s|@DATA_DIR@|$DATA_DIR|; s|@USER@|$SVC_USER|" \
    "$BUNDLE/systemd/broccoli-worker.service.tmpl" > /etc/systemd/system/broccoli-worker.service
  install -d /etc/systemd/system/broccoli-worker.service.d
  cp "$BUNDLE/systemd/10-isolate.conf" /etc/systemd/system/broccoli-worker.service.d/10-isolate.conf

  start_worker_units
  echo ">> worker ${wid:+'$wid' }up, judging for infra at $BROCCOLI_INFRA_HOST"
}

ROTATE_SECRETS=0
for arg in "${@:2}"; do
  case "$arg" in
    --rotate-secrets) ROTATE_SECRETS=1 ;;
    *) die "unknown argument: $arg" ;;
  esac
done

need_root
case "$ROLE" in
  infra-server) install_infra_server ;;
  worker)       install_worker ;;
  *) die "usage: sudo ./install-native.sh {infra-server [--rotate-secrets]|worker}" ;;
esac
