#!/usr/bin/env bash
# export-problems.sh and import-problems.sh run `broccoli-server bundle` on
# this node's server: the native binary from its data directory, or the one
# running server container through `docker exec -i`, streaming the archive
# through stdout and stdin. A failed export leaves no archive behind.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
fail() { echo "FAIL: $*"; exit 1; }
mkdir -p "$T/bin"
export PATH="$T/bin:$PATH" BROCCOLI_DATA_DIR="$T/none"

# A docker that runs one server container: `exec` records its arguments and
# echoes stdin back for import, or prints an archive for export.
cat > "$T/bin/docker" <<'E'
#!/usr/bin/env bash
case "$1" in
  ps) printf '%s\n' ${FAKE_CONTAINERS-c0ffee} ;;
  exec)
    shift; printf '%s\n' "$*" > "$FAKE_LOG"
    case "$*" in
      *"bundle export"*) [ -n "${FAKE_FAIL:-}" ] && { echo "error: missing blob" >&2; exit 1; }; printf 'ARCHIVE' ;;
      *"bundle import"*) cat > "$FAKE_LOG.stdin" ;;
    esac ;;
esac
E
chmod +x "$T/bin/docker"
export FAKE_LOG="$T/log"

bash "$here/../export-problems.sh" --contest 7 --with-secrets --out "$T/c.tar.gz" >/dev/null
[ "$(cat "$T/c.tar.gz")" = ARCHIVE ] || fail "export did not write the server's stdout"
grep -qx -- "-i c0ffee /usr/local/bin/broccoli-server bundle export --contest 7 --with-secrets" "$T/log" \
  || fail "export ran: $(cat "$T/log")"

FAKE_FAIL=1 bash "$here/../export-problems.sh" --out "$T/bad.tar.gz" 2>/dev/null && fail "a failed export succeeded"
[ ! -e "$T/bad.tar.gz" ] || fail "a failed export left a partial archive"

bash "$here/../import-problems.sh" "$T/c.tar.gz" --replace-problems
grep -qx -- "-i c0ffee /usr/local/bin/broccoli-server bundle import --replace-problems" "$T/log" \
  || fail "import ran: $(cat "$T/log")"
[ "$(cat "$T/log.stdin")" = ARCHIVE ] || fail "import did not stream the archive on stdin"

out="$(FAKE_CONTAINERS='a b' bash "$here/../import-problems.sh" "$T/c.tar.gz" 2>&1)" && fail "two containers accepted"
grep -q BROCCOLI_SERVER_CONTAINER <<<"$out" || fail "no hint for several containers: $out"

# A native install: the binary runs from the data directory.
mkdir -p "$T/data/target/release" "$T/data/config"
: > "$T/data/config/config.toml"
cat > "$T/data/target/release/server" <<'E'
#!/usr/bin/env bash
printf '%s %s\n' "$PWD" "$*" > "$FAKE_LOG"
E
chmod +x "$T/data/target/release/server"
BROCCOLI_DATA_DIR="$T/data" bash "$here/../export-problems.sh" --problems 1,2 --out "$T/n.tar.gz" >/dev/null
grep -qx "$T/data bundle export --problems 1,2" "$T/log" || fail "native export ran: $(cat "$T/log")"

echo "PASS: contest scripts run the server's bundle command, native or in its container"
