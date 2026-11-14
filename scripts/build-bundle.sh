#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C
export LANG=C
export COPYFILE_DISABLE=1

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <version-tag>  e.g. v0.3.0" >&2
  exit 64
fi

VERSION="$1"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$ROOT/dist"
BUNDLE_NAME="broccoli-platform-$VERSION"
WORK="$DIST/$BUNDLE_NAME"

SERVER_IMAGE="${BROCCOLI_SERVER_IMAGE:-broccoli-server:$VERSION}"
WORKER_BASE_IMAGE="${BROCCOLI_WORKER_BASE_IMAGE:-broccoli-worker:$VERSION-base}"
WORKER_ICPC_IMAGE="${BROCCOLI_WORKER_ICPC_IMAGE:-${BROCCOLI_WORKER_IMAGE:-broccoli-worker:$VERSION-icpc}}"
WORKER_FULL_IMAGE="${BROCCOLI_WORKER_FULL_IMAGE:-broccoli-worker:$VERSION-full}"
POSTGRES_IMAGE="${POSTGRES_IMAGE:-postgres:18-alpine}"
REDIS_IMAGE="${REDIS_IMAGE:-redis:7-alpine}"
SEAWEEDFS_IMAGE="${SEAWEEDFS_IMAGE:-chrislusf/seaweedfs:4.15}"
CADDY_IMAGE="${CADDY_IMAGE:-caddy:2-alpine}"
STRESS_BIN="${STRESS_BIN:-$ROOT/target/release/broccoli-stress-test}"
IMAGE_PLATFORM="${BROCCOLI_IMAGE_PLATFORM:-}"
PULL_IMAGES="${BROCCOLI_PULL_IMAGES:-auto}"
PYTHON_BIN="${PYTHON_BIN:-}"

if [[ -z "$PYTHON_BIN" ]]; then
  for candidate in python3.12 python3.11 python3; do
    if command -v "$candidate" >/dev/null 2>&1 && "$candidate" - <<'PY' >/dev/null 2>&1; then
import tomllib
PY
      PYTHON_BIN="$candidate"
      break
    fi
  done
fi

if [[ -z "$PYTHON_BIN" ]]; then
  echo "python 3.11+ with tomllib is required; set PYTHON_BIN=/path/to/python if needed" >&2
  exit 69
fi

rm -rf "$WORK"
mkdir -p "$WORK/images" "$WORK/stress-test" "$WORK/plugins"

copy_tree() {
  src="$1"
  dest="$2"
  mkdir -p "$(dirname "$dest")"
  tar \
    --exclude='.git' \
    --exclude='.DS_Store' \
    --exclude='target' \
    --exclude='node_modules' \
    --exclude='dist' \
    --exclude='build' \
    -C "$(dirname "$src")" -cf - "$(basename "$src")" | \
    tar -C "$(dirname "$dest")" -xf -
  if [[ "$(basename "$src")" != "$(basename "$dest")" ]]; then
    rm -rf "$dest"
    mv "$(dirname "$dest")/$(basename "$src")" "$dest"
  fi
}


local_image_matches_platform() {
  image="$1"
  platform="$2"
  [ -n "$platform" ] || return 0

  inspected_platform="$(docker image inspect \
    --format '{{.Os}}/{{.Architecture}}{{if .Variant}}/{{.Variant}}{{end}}' \
    "$image" 2>/dev/null || true)"
  [ "$inspected_platform" = "$platform" ]
}

save_image() {
  image="$1"
  out="$2"
  echo "saving $image"
  case "$PULL_IMAGES" in
    1|true|yes) should_pull=1 ;;
    0|false|no) should_pull=0 ;;
    auto)
      if docker image inspect "$image" >/dev/null 2>&1 && local_image_matches_platform "$image" "$IMAGE_PLATFORM"; then
        should_pull=0
      else
        should_pull=1
      fi
      ;;
    *) echo "BROCCOLI_PULL_IMAGES must be auto, true, or false" >&2; exit 64 ;;
  esac

  if [ "$should_pull" = 1 ]; then
    if [ -n "$IMAGE_PLATFORM" ]; then
      docker pull --platform "$IMAGE_PLATFORM" "$image" >/dev/null
    else
      docker pull "$image" >/dev/null
    fi
  fi
  docker save "$image" | gzip -9 > "$out"
}

save_image "$SERVER_IMAGE" "$WORK/images/server.tar.gz"
save_image "$WORKER_BASE_IMAGE" "$WORK/images/worker-base.tar.gz"
save_image "$WORKER_ICPC_IMAGE" "$WORK/images/worker-icpc.tar.gz"
save_image "$WORKER_FULL_IMAGE" "$WORK/images/worker-full.tar.gz"
save_image "$POSTGRES_IMAGE" "$WORK/images/postgres.tar.gz"
save_image "$REDIS_IMAGE" "$WORK/images/redis.tar.gz"
save_image "$SEAWEEDFS_IMAGE" "$WORK/images/seaweedfs.tar.gz"
save_image "$CADDY_IMAGE" "$WORK/images/caddy.tar.gz"

if [[ "${BROCCOLI_SKIP_PLUGIN_BUILD:-false}" != "true" ]]; then
  "$ROOT/scripts/build-plugins.sh"
fi

for file in \
  docker-compose.infra.yaml.template \
  docker-compose.server.yaml.template \
  docker-compose.worker.yaml.template \
  docker-compose.gateway.yaml.template \
  docker-compose.single-host.yaml.template \
  docker-compose.single-host.object-storage.yaml.template \
  .env.example \
  .env.infra.example \
  .env.server.example \
  .env.worker.example \
  .env.gateway.example; do
  cp "$ROOT/release/$file" "$WORK/$file"
done
cp "$ROOT/release/install.sh" "$WORK/install.sh"
cp "$ROOT/release/enable-plugin.sh" "$WORK/enable-plugin.sh"
# Worker CPU isolation (docs/operator-runbook.md) and the offline contest
# helpers (problem export/import, worker bring-up) run from the bundle too.
cp "$ROOT/release/setup-fairness.sh" "$WORK/setup-fairness.sh"
copy_tree "$ROOT/release/contest" "$WORK/contest"
rm -rf "$WORK/contest/test"
mkdir -p "$WORK/lib"
cp "$ROOT"/release/lib/*.sh "$WORK/lib/"
cp "$ROOT/docker-entrypoint-worker.sh" "$WORK/docker-entrypoint-worker.sh"
copy_tree "$ROOT/release/examples" "$WORK/examples"
copy_tree "$ROOT/release/docs" "$WORK/docs"
cp "$ROOT/release/README.md" "$WORK/README.md"
# The plugin SDKs with their crates.io closure, for plugin authors offline.
"$ROOT/scripts/stage-sdk.sh" "$WORK"

"$PYTHON_BIN" - "$WORK" "$VERSION" <<'PY'
from pathlib import Path
import sys

work = Path(sys.argv[1])
version = sys.argv[2]

for path in [
    work / "install.sh",
    work / ".env.example",
    work / ".env.infra.example",
    work / ".env.worker.example",
    work / ".env.server.example",
    work / ".env.gateway.example",
    work / "README.md",
    work / "examples" / "Dockerfile.worker.custom",
]:
    if not path.exists():
        continue
    text = path.read_text()
    text = text.replace('BUNDLE_VERSION_DEFAULT="v0.3.0"', f'BUNDLE_VERSION_DEFAULT="{version}"')
    text = text.replace("v0.3.0", version)
    path.write_text(text)
PY

# Every plugin under plugins/ ships, staged by its own [bundle] mode (see
# release/lib/plugins.sh).
# shellcheck source=../release/lib/plugins.sh
. "$ROOT/release/lib/plugins.sh"
stage_bundle_plugins "$ROOT/plugins" "$WORK"

"$PYTHON_BIN" - "$WORK" <<'PY'
from pathlib import Path
import sys
import tomllib

work = Path(sys.argv[1])
missing: list[str] = []

manifests = sorted(work.glob("plugins/*/plugin.toml")) + sorted(
    work.glob("plugins-available/*/plugin.toml")
)
for manifest_path in manifests:
    plugin_dir = manifest_path.parent
    manifest = tomllib.loads(manifest_path.read_text())
    server = manifest.get("server")
    if isinstance(server, dict) and server.get("entry"):
        entry = plugin_dir / server["entry"]
        if not entry.is_file():
            missing.append(f"{plugin_dir.name}: missing server entry {server['entry']}")

    web = manifest.get("web")
    if isinstance(web, dict):
        root = web.get("root")
        if root:
            web_root = plugin_dir / root
            entry = web.get("entry")
            if entry and not (web_root / entry).is_file():
                missing.append(f"{plugin_dir.name}: missing web entry {root}/{entry}")
            for css in web.get("css", []):
                if not (web_root / css).is_file():
                    missing.append(f"{plugin_dir.name}: missing web css {root}/{css}")

if missing:
    print("plugin bundle validation failed:", file=sys.stderr)
    for item in missing:
        print(f"  - {item}", file=sys.stderr)
    sys.exit(1)
PY

if [[ ! -x "$STRESS_BIN" ]]; then
  cargo build -p stress-test --release --locked
fi
cp "$STRESS_BIN" "$WORK/stress-test/broccoli-stress-test"
cp "$ROOT/packages/stress-test/README.md" "$WORK/stress-test/README.md"

tar --no-xattrs -C "$DIST" -czf "$DIST/$BUNDLE_NAME.tar.gz" "$BUNDLE_NAME"
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$DIST" && sha256sum "$BUNDLE_NAME.tar.gz" > "$BUNDLE_NAME.tar.gz.sha256")
else
  (cd "$DIST" && shasum -a 256 "$BUNDLE_NAME.tar.gz" > "$BUNDLE_NAME.tar.gz.sha256")
fi

echo "$DIST/$BUNDLE_NAME.tar.gz"
