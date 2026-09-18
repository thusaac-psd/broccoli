#!/usr/bin/env bash
# Checks Rust formatting for every plugin crate under `plugins/`.
#
# See scripts/test-plugins.sh for the rationale: the root Cargo.toml excludes
# `plugins` from the main workspace, so `cargo fmt --all --check` at the repo
# root never looks at plugin crates. Crates are discovered by searching for
# manifests (depth-agnostic, `target/` pruned) rather than listed, so a newly
# added plugin -- including a nested one like plugins/print/client -- is
# picked up automatically.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PLUGINS_DIR="${ROOT_DIR}/plugins"
cd "${ROOT_DIR}"

if [[ ! -d "${PLUGINS_DIR}" ]]; then
  echo "no plugins directory at ${PLUGINS_DIR}" >&2
  exit 1
fi

checked_any=0

while IFS= read -r -d '' manifest; do
  plugin_name="${manifest#"${ROOT_DIR}/"}"
  plugin_name="${plugin_name%/Cargo.toml}"
  echo "==> Checking formatting for plugin: ${plugin_name}"
  cargo fmt --manifest-path "${manifest}" --all --check
  checked_any=1
done < <(find "${PLUGINS_DIR}" -name target -prune -o -name Cargo.toml -print0 | sort -z)

if [[ ${checked_any} -eq 0 ]]; then
  echo "error: no plugin crates were found to check" >&2
  exit 1
fi
