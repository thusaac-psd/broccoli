#!/usr/bin/env bash
# Builds every plugin in `plugins/*` that has buildable server or web assets in
# plugin.toml. The CLI copies produced WASM modules to manifest entry paths and
# builds plugin frontend bundles into their manifest web roots. Plugin
# frontends are pnpm workspace members, so one root install covers them all.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PLUGINS_DIR="${ROOT_DIR}/plugins"

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

for plugin_dir in "${PLUGINS_DIR}"/*/; do
  plugin_name=$(basename "${plugin_dir}")
  manifest="${plugin_dir}plugin.toml"

  if [[ ! -f "${manifest}" ]]; then
    continue
  fi

  if ! grep -Eq '^\[(server|web)\][[:space:]]*$' "${manifest}"; then
    continue
  fi

  echo "==> Building plugin: ${plugin_name}"
  (cd "${ROOT_DIR}" && cargo run -p broccoli-dev-cli --locked -- plugin build "plugins/${plugin_name}" --release)
  built_any=1
done

if [[ ${built_any} -eq 0 ]]; then
  echo "warning: no plugins with [server] or [web] entries were built" >&2
fi
