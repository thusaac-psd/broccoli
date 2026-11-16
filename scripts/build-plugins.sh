#!/usr/bin/env bash
# Builds every plugin in `plugins/*` that has buildable server or web assets in
# plugin.toml, and every server test fixture plugin (a plugin.toml next to a
# Cargo.toml under packages/server/tests). The CLI copies produced WASM modules
# to manifest entry paths and builds plugin frontend bundles into their
# manifest web roots. Plugin frontends are pnpm workspace members, so one root
# install covers them all.
#
# The WASM modules are build output and git-ignored, so the server's tests
# always load plugins and fixtures built from the current SDK sources.
#
# Every plugin crate builds into the one target directory `target/plugins`, so
# a cache of `target/` (CI's rust-cache) covers plugin builds too.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PLUGINS_DIR="${ROOT_DIR}/plugins"
FIXTURES_DIR="${ROOT_DIR}/packages/server/tests"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT_DIR}/target/plugins}"

if [[ ! -d "${PLUGINS_DIR}" ]]; then
  echo "no plugins directory at ${PLUGINS_DIR}" >&2
  exit 1
fi

shopt -s nullglob
built_any=0
has_web=0

for plugin_dir in "${PLUGINS_DIR}"/*/; do
  manifest="${plugin_dir}plugin.toml"
  if [[ -f "${manifest}" ]] && grep -Eq '^\[web\][[:space:]]*$' "${manifest}"; then
    has_web=1
    break
  fi
done

if [[ ${has_web} -eq 1 ]]; then
  echo "==> Building shared web SDK"
  (
    cd "${ROOT_DIR}"
    pnpm install --frozen-lockfile
    pnpm --filter @broccoli/web-sdk build
  )
fi

build() {
  local dir="${1%/}"
  echo "==> Building plugin: ${dir#"${ROOT_DIR}/"}"
  (cd "${ROOT_DIR}" && cargo run -p broccoli-dev-cli --locked -- plugin build "${dir}" --release)
  built_any=1
}

for plugin_dir in "${PLUGINS_DIR}"/*/; do
  manifest="${plugin_dir}plugin.toml"
  if [[ -f "${manifest}" ]] && grep -Eq '^\[(server|web)\][[:space:]]*$' "${manifest}"; then
    build "${plugin_dir}"
  fi
done

while IFS= read -r -d '' manifest; do
  fixture_dir="$(dirname "${manifest}")"
  if [[ -f "${fixture_dir}/Cargo.toml" ]]; then
    build "${fixture_dir}"
  fi
done < <(find "${FIXTURES_DIR}" -name target -prune -o -name plugin.toml -print0 | sort -z)

if [[ ${built_any} -eq 0 ]]; then
  echo "warning: no plugins with [server] or [web] entries were built" >&2
fi
