#!/usr/bin/env bash
# Assemble a self-contained NATIVE contest bundle from a built Broccoli host
# (the box that has compiled server/worker/broccoli-compare + installed isolate,
# weed, and plugins). Produces one tar.gz that install-native.sh can deploy onto
# fresh Ubuntu 22.04 LAN boxes with no internet and no Docker.
#
# Run ON the build/staging box:
#   ./build-native-bundle.sh [version]
# Env: BROCCOLI_ROOT (default /data/broccoli), OUT_DIR (default $PWD).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # release/native in the repo tree
CONTEST_DIR="$(cd "$HERE/../contest" && pwd)"
ROOT="${BROCCOLI_ROOT:-/data/broccoli}"
VERSION="${1:-$(date -u +%Y%m%d)}"
OUT_DIR="${OUT_DIR:-$PWD}"
NAME="broccoli-native-$VERSION"
WORK="$(mktemp -d)/$NAME"
MUSL_TARGET="x86_64-unknown-linux-musl"   # contestant CLI is musl-static: runs on any distro / glibc 2.31
trap 'rm -rf "$(dirname "$WORK")"' EXIT

die(){ echo "error: $*" >&2; exit 1; }
need(){ [ -e "$1" ] || die "missing required artifact: $1"; }

# shellcheck source=../lib/plugins.sh
. "$HERE/../lib/plugins.sh"

mkdir -p "$WORK"/{bin,cli,plugins,web,scripts,systemd,config,contest}

echo ">> collecting binaries"
for b in server worker broccoli-compare; do
  need "$ROOT/target/release/$b"; install -m755 "$ROOT/target/release/$b" "$WORK/bin/$b"
done
need /usr/local/bin/weed
# Ship the SeaweedFS release the Docker templates run (the SEAWEEDFS_IMAGE tag
# in .env.infra.example), so broccoli-weed.service's flags are known to work.
# BROCCOLI_WEED_ANY_VERSION=1 ships another one; install-native.sh then drops
# any flag that binary does not know.
weed_pin="$(sed -n 's/^SEAWEEDFS_IMAGE=.*:\([^:]*\)$/\1/p' "$HERE/../.env.infra.example")"
weed_have="$(/usr/local/bin/weed version 2>/dev/null | awk '/^version/ { print $3; exit }')"
if [ "$weed_have" != "$weed_pin" ] && [ "${BROCCOLI_WEED_ANY_VERSION:-0}" != 1 ]; then
  die "/usr/local/bin/weed is SeaweedFS ${weed_have:-unknown}, not the pinned $weed_pin; install $weed_pin or set BROCCOLI_WEED_ANY_VERSION=1"
fi
install -m755 /usr/local/bin/weed "$WORK/bin/weed"
need /usr/local/bin/isolate; install -m755 /usr/local/bin/isolate "$WORK/bin/isolate"  # setuid set at install time

echo ">> collecting contestant CLI (broccoli, musl-static) for distribution to contestant machines"
# Client binary contestants run on their own LAN machines (not a server service).
# install-native.sh does NOT install it; the contest admin copies cli/broccoli out.
# It ships MUSL-STATIC so it runs on any distro (incl. glibc 2.31 / bare boxes)
# with no host-glibc dependency. Build it before bundling (matches CI):
#   rustup target add "$MUSL_TARGET" && sudo apt-get install -y musl-tools
#   cargo build -p broccoli-contestant-cli --profile release-cli --locked --target "$MUSL_TARGET"
CLI_MUSL="$ROOT/target/$MUSL_TARGET/release-cli/broccoli"
need "$CLI_MUSL"; install -m755 "$CLI_MUSL" "$WORK/cli/broccoli"

echo ">> collecting print-station client (print-client) if built"
# Separate crate (plugins/print/client). Deployed to machines with a printer;
# polls the server, renders highlighted PDFs (CJK font bundled), runs the print
# command. Build before bundling: (cd plugins/print/client && cargo build --release)
PRINTCLIENT="$ROOT/plugins/print/client/target/release/print-client"
if [ -f "$PRINTCLIENT" ]; then
  install -m755 "$PRINTCLIENT" "$WORK/cli/print-client"
  cat > "$WORK/cli/print-client.toml.example" <<'EOF'
station = "station-1"
poll_interval_secs = 3
max_pages = 20
paper = "A4"
font_size = 9.0
banner = ""

[[server]]
url = "http://<box1-LAN-IP>"
token = "<station-token: set in the print plugin's config, scope=plugin>"

[[printer]]
name = "main"
# real CUPS printer:  command = "lp -d <cups-printer-name> {file}"
# folder sink (test): command = "folder:/var/spool/broccoli-print"
command = "lp -d main {file}"
EOF
  echo "  print-client: included (+ print-client.toml.example)"
else
  echo "  NOTE: print-client not built (print stations need it) -- (cd plugins/print/client && cargo build --release)"
fi

echo ">> collecting plugins (every plugin, staged by its [bundle] mode)"
stage_bundle_plugins "$ROOT/plugins" "$WORK"

echo ">> collecting server web frontend"
need "$ROOT/packages/web/build/client"; cp -a "$ROOT/packages/web/build/client" "$WORK/web/client"

echo ">> collecting isolate cgroup-setup script (authoritative copy from box)"
need "$ROOT/scripts/fix-live-ubuntu-isolate.sh"
install -m755 "$ROOT/scripts/fix-live-ubuntu-isolate.sh" "$WORK/scripts/fix-live-ubuntu-isolate.sh"

echo ">> collecting installer + templates + contest tooling"
install -m755 "$HERE/install-native.sh" "$WORK/install-native.sh"
install -m755 "$HERE/../enable-plugin.sh" "$WORK/enable-plugin.sh"
mkdir -p "$WORK/lib"; cp "$HERE"/../lib/*.sh "$WORK/lib/"
install -m755 "$HERE/stage-toolchain.sh" "$WORK/stage-toolchain.sh"
install -m755 "$HERE/live-boot-preflight.sh" "$WORK/live-boot-preflight.sh"
# pre-staged offline C/C++/Python toolchain .deb sets for live-boot workers, if any.
# install-native.sh (worker) auto-installs the set matching the box's Ubuntu version.
if [ -d "$HERE/toolchain" ]; then
  cp -a "$HERE/toolchain" "$WORK/toolchain"
  echo "  toolchain sets bundled: $(ls "$WORK/toolchain" 2>/dev/null | tr '\n' ' ')"
else
  echo "  NOTE: no pre-staged toolchain/ -- live-boot workers install build-essential via apt/LAN mirror, or run stage-toolchain.sh"
fi
cp "$HERE"/systemd/* "$WORK/systemd/"
cp "$HERE"/config/*  "$WORK/config/"
[ -f "$HERE/README.md" ] && cp "$HERE/README.md" "$WORK/README.md"
for f in s3copy.py export-problems.sh import-problems.sh README.md; do
  [ -f "$CONTEST_DIR/$f" ] && cp "$CONTEST_DIR/$f" "$WORK/contest/$f"
done
chmod +x "$WORK"/contest/*.sh 2>/dev/null || true

echo ">> validating bundle completeness"
for b in server worker broccoli-compare weed isolate; do need "$WORK/bin/$b"; done
need "$WORK/cli/broccoli"                                          # contestant CLI (required deliverable)
need "$WORK/web/client"; need "$WORK/scripts/fix-live-ubuntu-isolate.sh"
need "$WORK/config/config.toml.tmpl"; need "$WORK/install-native.sh"
miss=0
for p in standard-languages standard-checkers batch-evaluator communication-evaluator icpc ioi; do
  entry="$(grep -m1 -E '^[[:space:]]*entry[[:space:]]*=' "$WORK/plugins/$p/plugin.toml" 2>/dev/null | sed -E 's/.*=[[:space:]]*"//; s/".*//')"
  [ -f "$WORK/plugins/$p/$entry" ] || { echo "  MISSING judge wasm: $p/$entry"; miss=1; }
done
[ "$miss" = 0 ] || die "required judge plugin wasm missing -- aborting"
# every i18n file a plugin.toml [translations] references must be in the bundle,
# or the server fails plugin activation at boot (see build-native-bundle history).
i18n_miss=0
for ptoml in "$WORK"/plugins/*/plugin.toml; do
  pdir="$(dirname "$ptoml")"
  while IFS= read -r rel; do
    [ -z "$rel" ] && continue
    [ -f "$pdir/$rel" ] || { echo "  MISSING i18n: $(basename "$pdir")/$rel"; i18n_miss=1; }
  done < <(grep -oE '"i18n/[^"]+\.toml"' "$ptoml" 2>/dev/null | tr -d '"')
done
[ "$i18n_miss" = 0 ] || die "plugin translation file(s) missing -- aborting (would break plugin activation)"
# print plugin: backend wasm is required (contest printing); staff web UI is best-effort
[ -f "$WORK/plugins/print/print_plugin.wasm" ] && echo "  print backend wasm: included" || echo "  WARN: print backend wasm MISSING"
[ -f "$WORK/plugins/print/frontend/dist/index.js" ] && echo "  print staff web UI: included" \
  || echo "  NOTE: print staff web UI not built (backend printing still works; build frontend/dist on a connected machine for the queue panel)"

echo ">> packing"
tar --no-xattrs --exclude='.DS_Store' -C "$(dirname "$WORK")" -czf "$OUT_DIR/$NAME.tar.gz" "$NAME"
( cd "$OUT_DIR" && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256" 2>/dev/null || shasum -a 256 "$NAME.tar.gz" > "$NAME.tar.gz.sha256" )
echo ">> wrote $OUT_DIR/$NAME.tar.gz ($(du -h "$OUT_DIR/$NAME.tar.gz" | cut -f1))"
echo ">> sha256: $(cut -d' ' -f1 "$OUT_DIR/$NAME.tar.gz.sha256")"
