#!/usr/bin/env bash
# Runs the Rust test suite for every plugin crate under `plugins/`.
#
# The root Cargo.toml lists `plugins` in its `exclude` array (those crates
# target wasm32 and pin their own `[workspace]` root so they build standalone
# even when nested inside another workspace), so `cargo test --workspace`
# skips every plugin crate. This script discovers plugin crates by searching
# for manifests rather than keeping a list, so a new plugin is covered
# without editing it.
#
# The search is depth-agnostic rather than a one-level `plugins/*/Cargo.toml`
# glob, because not every plugin crate sits at the top level (for example the
# native binary crate plugins/print/client). `target/` is pruned so build
# artefacts' vendored manifests aren't mistaken for plugin crates.
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
# One target directory for every plugin crate (see scripts/build-plugins.sh),
# so the dependencies they share build once.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT_DIR}/target/plugins}"

if [[ ! -d "${PLUGINS_DIR}" ]]; then
  echo "no plugins directory at ${PLUGINS_DIR}" >&2
  exit 1
fi

tested_any=0

while IFS= read -r -d '' manifest; do
  plugin_name="${manifest#"${ROOT_DIR}/"}"
  plugin_name="${plugin_name%/Cargo.toml}"
  echo "==> Testing plugin: ${plugin_name}"
  cargo test --manifest-path "${manifest}" --locked -- --test-threads=4
  tested_any=1
done < <(find "${PLUGINS_DIR}" -name target -prune -o -name Cargo.toml -print0 | sort -z)

if [[ ${tested_any} -eq 0 ]]; then
  echo "error: no plugin crates were found to test" >&2
  exit 1
fi
