#!/usr/bin/env bash
# Runs Clippy (`-D warnings`) for every plugin crate under `plugins/`.
#
# See scripts/test-plugins.sh for the rationale: the root Cargo.toml excludes
# `plugins` from the main workspace, so `cargo clippy --workspace` at the repo
# root never looks at plugin crates. Crates are discovered by searching for
# manifests (depth-agnostic, `target/` pruned) rather than listed, so a newly
# added plugin -- including a nested one like plugins/print/client -- is
# picked up automatically.
#
# Deliberately invoked from the repo root and never `cd`s into a plugin
# directory: some plugin crates carry a local `.cargo/config.toml` that pins
# a non-host default `[build] target` (e.g. standard-languages defaults to
# wasm32-wasip1). Cargo's config-file discovery walks up from the *current
# working directory*, not from `--manifest-path`, so running from the repo
# root (which carries no such override) keeps Clippy on the host target.
#
# `--all-targets` lints test code too, not just the library, since a plugin
# crate's own #[cfg(test)] unit tests are as much a maintained artefact as
# its production code.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PLUGINS_DIR="${ROOT_DIR}/plugins"
cd "${ROOT_DIR}"

if [[ ! -d "${PLUGINS_DIR}" ]]; then
  echo "no plugins directory at ${PLUGINS_DIR}" >&2
  exit 1
fi

linted_any=0

while IFS= read -r -d '' manifest; do
  plugin_name="${manifest#"${ROOT_DIR}/"}"
  plugin_name="${plugin_name%/Cargo.toml}"
  echo "==> Linting plugin: ${plugin_name}"
  cargo clippy --manifest-path "${manifest}" --all-targets --locked -- -D warnings
  linted_any=1
done < <(find "${PLUGINS_DIR}" -name target -prune -o -name Cargo.toml -print0 | sort -z)

if [[ ${linted_any} -eq 0 ]]; then
  echo "error: no plugin crates were found to lint" >&2
  exit 1
fi
