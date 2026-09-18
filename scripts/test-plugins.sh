#!/usr/bin/env bash
# Runs the Rust test suite for every plugin crate under `plugins/*`.
#
# The root Cargo.toml lists `plugins` in its `exclude` array (those crates
# target wasm32 and pin their own `[workspace]` root so they build standalone
# even when nested inside another workspace), which means
# `cargo test --workspace` silently skips every plugin crate. This script
# iterates over every top-level plugin crate directory instead of hardcoding
# a crate list, so a newly added plugin is picked up automatically -- a
# hardcoded list is exactly what let this gap open in the first place.
#
# Deliberately invoked from the repo root and never `cd`s into a plugin
# directory: some plugin crates carry a local `.cargo/config.toml` that pins
# a non-host default `[build] target` (e.g. standard-languages defaults to
# wasm32-wasip1, for a convenient plain `cargo build` -> wasm artefact).
# Cargo's config-file discovery walks up from the *current working
# directory*, not from `--manifest-path`, so running from the repo root
# (which carries no such override) keeps `cargo test` on the host target,
# where the produced test binaries can actually execute.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PLUGINS_DIR="${ROOT_DIR}/plugins"
cd "${ROOT_DIR}"

if [[ ! -d "${PLUGINS_DIR}" ]]; then
  echo "no plugins directory at ${PLUGINS_DIR}" >&2
  exit 1
fi

shopt -s nullglob
tested_any=0

for manifest in "${PLUGINS_DIR}"/*/Cargo.toml; do
  plugin_name=$(basename "$(dirname "${manifest}")")
  echo "==> Testing plugin: ${plugin_name}"
  cargo test --manifest-path "${manifest}" --locked -- --test-threads=4
  tested_any=1
done

if [[ ${tested_any} -eq 0 ]]; then
  echo "error: no plugin crates were found to test" >&2
  exit 1
fi
