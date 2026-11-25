#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT

# fake WORKING docker on PATH
mkdir -p "$tmp/bin"
cat > "$tmp/bin/docker" <<'E'
#!/usr/bin/env bash
case "$1" in info) exit 0;; compose) [ "$2" = version ] && exit 0; exit 0;; *) exit 0;; esac
E
chmod +x "$tmp/bin/docker"

# fake bundle with a valid manifest
mkdir -p "$tmp/bundle/compose"; echo img > "$tmp/bundle/images.txt"
( cd "$here/.." && . lib/manifest.sh && manifest_generate "$tmp/bundle" )
# server secret dir with a root.key
sec="$tmp/sec"; mkdir -p "$sec"; echo k > "$sec/root.key"

run() { PATH="$tmp/bin:$PATH" bash -c '. '"$here"'/../lib/preflight.sh; preflight_run "$@"' _ "$@"; }

# server with good runtime + bundle + secret -> rc 0
if ! out="$(run server "$tmp/bundle" "$sec")"; then echo "FAIL: good server preflight returned nonzero"; echo "$out"; exit 1; fi
grep -q '^PASS: container engine: docker' <<<"$out" || { echo "FAIL: engine pass line missing"; exit 1; }

# server missing secret -> FAIL rc1
set +e; out="$(run server "$tmp/bundle" "$tmp/nope")"; rc=$?; set -e
[ "$rc" = 1 ] || { echo "FAIL: missing secret should fail (rc=$rc)"; exit 1; }
grep -q '^FAIL:' <<<"$out" || { echo "FAIL: no FAIL line for missing secret"; exit 1; }

# nonexistent bundle but valid secret -> FAIL rc1, role check still runs
set +e; out="$(run server "$tmp/no-such-bundle" "$sec")"; rc=$?; set -e
[ "$rc" = 1 ] || { echo "FAIL: nonexistent bundle should fail (rc=$rc)"; exit 1; }
grep -q '^FAIL: bundle integrity' <<<"$out" || { echo "FAIL: no bundle integrity FAIL line"; exit 1; }
grep -q '^PASS: server TLS material present' <<<"$out" || { echo "FAIL: server role check did not run"; exit 1; }

# no engine (empty PATH stubs failing) -> FAIL
mkdir -p "$tmp/nobin"
cat > "$tmp/nobin/docker" <<'E'
#!/usr/bin/env bash
exit 1
E
cat > "$tmp/nobin/podman" <<'E'
#!/usr/bin/env bash
exit 1
E
chmod +x "$tmp/nobin/docker" "$tmp/nobin/podman"
set +e
out="$(PATH="$tmp/nobin:$tmp/bin_missing:/usr/bin:/bin" bash -c '. '"$here"'/../lib/preflight.sh; preflight_run server "'"$tmp"'/bundle" "'"$sec"'"')"
rc=$?
set -e
[ "$rc" = 1 ] || { echo "FAIL: no-engine should fail (rc=$rc)"; exit 1; }
grep -q '^FAIL: no working docker' <<<"$out" || { echo "FAIL: no docker/podman FAIL line"; exit 1; }

# worker preflight PASSES with a cluster-secret sidecar present (4th arg)
cls="$tmp/cls"; mkdir -p "$cls"; : > "$cls/cluster-secrets.env"
set +e; out="$(run worker "$tmp/bundle" "" "$cls")"; rc=$?; set -e
[ "$rc" = 0 ] || { echo "FAIL: worker preflight failed despite cluster-secret present (rc=$rc)"; echo "$out"; exit 1; }
grep -q '^PASS: cluster-secret present' <<<"$out" || { echo "FAIL: no cluster-secret PASS line"; echo "$out"; exit 1; }

# worker preflight FAILS without a cluster-secret sidecar
set +e; out="$(run worker "$tmp/bundle" "" "")"; rc=$?; set -e
[ "$rc" = 1 ] || { echo "FAIL: worker preflight should fail without cluster-secret (rc=$rc)"; echo "$out"; exit 1; }
grep -q '^FAIL:' <<<"$out" || { echo "FAIL: expected a FAIL line for missing cluster-secret"; echo "$out"; exit 1; }

echo "PASS: preflight go/no-go"
