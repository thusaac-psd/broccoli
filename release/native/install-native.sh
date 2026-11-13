#!/usr/bin/env bash
# Native (non-Docker) installer for an offline Broccoli contest LAN on
# Ubuntu 22.04 (x86_64). Mirrors the battery-verified staging deployment:
# native Postgres + Redis + SeaweedFS + systemd-managed broccoli services,
# isolate sandbox with cgroup-v2 delegation. No Docker, no internet required
# (assumes pg/redis available via apt mirror or pre-installed).
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
gen(){ head -c "${1:-24}" /dev/urandom | base64 | tr -dc 'A-Za-z0-9' | head -c "${2:-32}"; }
# toml_string FILE KEY: the first `KEY = "value"` in FILE.
toml_string(){ sed -n "s/^$2 *= *\"\(.*\)\"\$/\1/p" "$1" | head -n 1; }
# toml_url_password FILE SCHEME: the password in the first SCHEME:// url.
toml_url_password(){ sed -n "s|^url *= *\"$2://[^:]*:\([^@]*\)@.*|\1|p" "$1" | head -n 1; }

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

apt_ensure(){ # install pkgs only if missing (works against a local apt mirror)
  local miss=()
  for p in "$@"; do dpkg -s "$p" >/dev/null 2>&1 || miss+=("$p"); done
  [ ${#miss[@]} -eq 0 ] && return 0
  echo ">> apt install: ${miss[*]}"
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${miss[@]}" \
    || die "apt install failed (${miss[*]}); on an airgap, point apt at a local mirror or pre-install these"
}

# Provision the worker's C/C++/Python toolchain for offline / live-boot boxes.
# Order: bundled .debs matching this Ubuntu version (stage-toolchain.sh) -> apt
# (online or LAN mirror). Then VERIFY with a real compile -- a worker that cannot
# compile C/C++ would wrongly CE every such submission, so we refuse to bring one
# up. Deliberate Python-only pool? set BROCCOLI_ALLOW_NO_CXX=1.
ensure_toolchain(){
  local ver debs rc=0 t
  ver="$(. /etc/os-release 2>/dev/null; echo "${VERSION_ID:-}")"
  debs="$BUNDLE/toolchain/$ver"
  # 1) offline: matching bundled .deb set, if shipped
  if ! dpkg -s build-essential >/dev/null 2>&1 && ls "$debs"/*.deb >/dev/null 2>&1; then
    echo ">> installing bundled toolchain for Ubuntu $ver ($debs)"
    # --skip-same-version: the set is the full closure, so a live Desktop's
    # already-present base libs (libc6, perl, tar, ...) are skipped; only the
    # missing toolchain packages (gcc/g++/*-dev/...) install. Offline-safe.
    dpkg -i --skip-same-version "$debs"/*.deb >/dev/null 2>&1 || apt-get install -fy >/dev/null 2>&1 || true
  fi
  # 2) still missing -> apt (online / local mirror)
  dpkg -s build-essential >/dev/null 2>&1 || \
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential python3 2>/dev/null || true
  command -v python3 >/dev/null 2>&1 || apt-get install -y -qq python3 2>/dev/null || true
  # 3) verify with a real compile (gcc + g++) and python3 presence
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
  Fix with ONE of:
    - bundled debs:  sudo dpkg -i $BUNDLE/toolchain/${ver:-<ver>}/*.deb   (stage on an online box: ./stage-toolchain.sh ${ver:-<ver>})
    - apt:           sudo apt-get install -y build-essential python3       (online or LAN mirror)
  then re-run: sudo ./install-native.sh worker
  (Deliberate Python-only worker pool? re-run with BROCCOLI_ALLOW_NO_CXX=1.)"
  fi
  echo ">> toolchain OK: $(gcc --version | head -1) / $(g++ --version | head -1) / $(python3 --version 2>&1)"
}

install_common_dirs(){
  install -d -o "$SVC_USER" -g "$SVC_USER" "$DATA_DIR" "$DATA_DIR/target/release" \
    "$DATA_DIR/config" "$DATA_DIR/logs" "$DATA_DIR/tools" "$DATA_DIR/scripts" "${DATA_DIR}-data"
}

# ---------------------------------------------------------------- infra-server
install_infra_server(){
  local host="${BROCCOLI_INFRA_HOST:-}"
  [ -n "$host" ] || die "set BROCCOLI_INFRA_HOST=<this box's LAN IP> for infra-server"
  case "${BROCCOLI__AUTH__SECURE_COOKIES:-false}" in
    true|false) ;;
    *) die "BROCCOLI__AUTH__SECURE_COOKIES must be true or false" ;;
  esac
  require_ubuntu; install_common_dirs
  apt_ensure postgresql postgresql-client redis-server

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
  sudo -u postgres psql -tc "SELECT 1 FROM pg_roles WHERE rolname='$DB_USER'" | grep -q 1 \
    || sudo -u postgres psql -c "CREATE ROLE $DB_USER LOGIN PASSWORD '$db_pw'"
  sudo -u postgres psql -c "ALTER ROLE $DB_USER PASSWORD '$db_pw'"
  sudo -u postgres psql -tc "SELECT 1 FROM pg_database WHERE datname='$DB_NAME'" | grep -q 1 \
    || sudo -u postgres createdb -O "$DB_USER" "$DB_NAME"
  local pgconf pghba subnet
  pgconf="$(ls /etc/postgresql/*/main/postgresql.conf | head -1)"
  pghba="$(dirname "$pgconf")/pg_hba.conf"
  subnet="${BROCCOLI_LAN_CIDR:-${host%.*}.0/24}"
  grep -q "^listen_addresses *= *'\*'" "$pgconf" || sed -i "s/^#\?listen_addresses.*/listen_addresses = '*'/" "$pgconf"
  # Replace the rule on every run, so a new LAN (rehearsal -> venue) is let in.
  sed -i '/# broccoli-lan$/d' "$pghba"
  echo "host $DB_NAME $DB_USER $subnet scram-sha-256 # broccoli-lan" >> "$pghba"
  systemctl restart postgresql

  # redis: LAN bind + auth
  echo ">> configuring redis"
  local rconf=/etc/redis/redis.conf
  sed -i "s/^bind .*/bind 127.0.0.1 $host/" "$rconf"
  sed -i "s/^# *requirepass .*/requirepass $redis_pw/; s/^requirepass .*/requirepass $redis_pw/" "$rconf"
  grep -q "^requirepass " "$rconf" || echo "requirepass $redis_pw" >> "$rconf"
  systemctl restart redis-server

  # seaweedfs s3 identity + service. SeaweedFS itself listens on loopback
  # only; workers reach just its credentialed S3 API, forwarded from
  # $host:8333 by a systemd socket (the filer would otherwise hand any
  # contestant on the LAN every testcase without credentials).
  echo ">> configuring seaweedfs"
  local proxyd
  proxyd="$(ls /usr/lib/systemd/systemd-socket-proxyd /lib/systemd/systemd-socket-proxyd 2>/dev/null | head -n 1)" || true
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
      -e "s|@SECURE_COOKIES@|${BROCCOLI__AUTH__SECURE_COOKIES:-false}|" \
      -e "s|@DATA_DIR@|$DATA_DIR|; s|@SERVER_ID@|server-1|; s|@WORKER_ID@|server-1|" \
      -e "s|@HTTP_HOST@|0.0.0.0|; s|@HTTP_PORT@|80|; s|@MAX_CONCURRENCY@|1|; s|@INFRA_HOST@|$host|" \
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
  echo ">> import problems with:  cd contest && ./import-problems.sh <archive>   (reads $CONN_ENV)"
}

# ---------------------------------------------------------------------- worker
install_worker(){
  [ -f "$CONN_ENV" ] || die "connection.env not found in $BUNDLE (copy it from the infra box)"
  # shellcheck disable=SC1090
  set -a; . "$CONN_ENV"; set +a
  local wid="${BROCCOLI__WORKER__ID:-$(node_id worker)}"
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
      -e "s|@SECURE_COOKIES@|false|" \
      -e "s|@DATA_DIR@|$DATA_DIR|; s|@SERVER_ID@|$wid|; s|@WORKER_ID@|$wid|" \
      -e "s|@HTTP_HOST@|127.0.0.1|; s|@HTTP_PORT@|8081|; s|@MAX_CONCURRENCY@|${BROCCOLI_MAX_CONCURRENCY:-8}|; s|@INFRA_HOST@|${BROCCOLI_INFRA_HOST:-127.0.0.1}|" \
      "$BUNDLE/config/config.toml.tmpl" > "$DATA_DIR/config/worker.toml"
  chown "$SVC_USER":"$SVC_USER" "$DATA_DIR/config/worker.toml"; chmod 600 "$DATA_DIR/config/worker.toml"

  # worker unit + isolate drop-in
  sed -e "s|@DATA_DIR@|$DATA_DIR|; s|@USER@|$SVC_USER|" \
    "$BUNDLE/systemd/broccoli-worker.service.tmpl" > /etc/systemd/system/broccoli-worker.service
  install -d /etc/systemd/system/broccoli-worker.service.d
  cp "$BUNDLE/systemd/10-isolate.conf" /etc/systemd/system/broccoli-worker.service.d/10-isolate.conf

  start_worker_units
  echo ">> worker '$wid' up, judging for infra at $BROCCOLI_INFRA_HOST"
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
