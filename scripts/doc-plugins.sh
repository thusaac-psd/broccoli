#!/usr/bin/env bash
# Builds rustdoc for every plugin crate under `plugins/`, with warnings denied.
#
# See scripts/test-plugins.sh for the rationale: the root Cargo.toml excludes
# `plugins` from the main workspace, so `cargo doc --workspace` at the repo
# root never documents a plugin crate. Crates are discovered by searching for
# manifests (depth-agnostic, `target/` pruned) rather than listed, so a newly
# added plugin -- including a nested one like plugins/print/client -- is
# picked up automatically.
#
# `RUSTDOCFLAGS="-D warnings"` matches what ci.yml's "Documentation" step sets
# for the workspace: without it, rustdoc only warns about a broken intra-doc
# link, and a doc comment that links a private or `#[cfg(test)]`-only item
# passes.
#
# Deliberately invoked from the repo root and never `cd`s into a plugin
# directory: some plugin crates carry a local `.cargo/config.toml` that pins
# a non-host default `[build] target` (e.g. standard-languages defaults to
# wasm32-wasip1). Cargo's config-file discovery walks up from the *current
# working directory*, not from `--manifest-path`, so running from the repo
# root (which carries no such override) keeps rustdoc on the host target.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PLUGINS_DIR="${ROOT_DIR}/plugins"
cd "${ROOT_DIR}"
# One target directory for every plugin crate (see scripts/build-plugins.sh),
# so the dependencies they share build once.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT_DIR}/target/plugins}"

if [[ ! -d "${PLUGINS_DIR}" ]]; then
  echo "no plugins directory at ${PLUGINS_DIR}" >&2
  exit 1
fi

export RUSTDOCFLAGS="${RUSTDOCFLAGS:--D warnings}"

documented_any=0

while IFS= read -r -d '' manifest; do
  plugin_name="${manifest#"${ROOT_DIR}/"}"
  plugin_name="${plugin_name%/Cargo.toml}"
  echo "==> Documenting plugin: ${plugin_name}"
  cargo doc --manifest-path "${manifest}" --no-deps --locked
  documented_any=1
done < <(find "${PLUGINS_DIR}" -name target -prune -o -name Cargo.toml -print0 | sort -z)

if [[ ${documented_any} -eq 0 ]]; then
  echo "error: no plugin crates were found to document" >&2
  exit 1
fi
