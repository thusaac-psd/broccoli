#!/usr/bin/env bash
# Checks Rust formatting for every plugin crate under `plugins/*`.
#
# See scripts/test-plugins.sh for the rationale: the root Cargo.toml excludes
# `plugins` from the main workspace, so `cargo fmt --all --check` at the repo
# root never looks at plugin crates. This iterates over every top-level
# plugin crate directory so a newly added plugin is picked up automatically.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PLUGINS_DIR="${ROOT_DIR}/plugins"
cd "${ROOT_DIR}"

if [[ ! -d "${PLUGINS_DIR}" ]]; then
  echo "no plugins directory at ${PLUGINS_DIR}" >&2
  exit 1
fi

shopt -s nullglob
checked_any=0

for manifest in "${PLUGINS_DIR}"/*/Cargo.toml; do
  plugin_name=$(basename "$(dirname "${manifest}")")
  echo "==> Checking formatting for plugin: ${plugin_name}"
  cargo fmt --manifest-path "${manifest}" --all --check
  checked_any=1
done

if [[ ${checked_any} -eq 0 ]]; then
  echo "error: no plugin crates were found to check" >&2
  exit 1
fi
