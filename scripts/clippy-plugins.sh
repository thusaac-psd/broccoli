#!/usr/bin/env bash
# Runs Clippy (`-D warnings`) for every plugin crate, for the target it ships
# as.
#
# See scripts/test-plugins.sh for the rationale: the root Cargo.toml excludes
# `plugins` from the main workspace, so `cargo clippy --workspace` at the repo
# root never looks at plugin crates. Crates are discovered by searching for
# manifests (depth-agnostic, `target/` pruned) rather than listed, so a newly
# added plugin -- including a nested one like plugins/print/client -- is
# picked up automatically.
#
# Two passes per crate under `plugins/`:
#
# - the host, `--all-targets`: a plugin crate's #[cfg(test)] unit tests and
#   integration tests compile only there, and they are as much a maintained
#   artefact as its production code;
# - `wasm32-wasip1`, for a crate that ships as a WASM backend (its directory's
#   `plugin.toml` declares `[server]`, so `broccoli-dev plugin build` builds it
#   for that target): code under `#[cfg(target_arch = "wasm32")]` compiles only
#   there, and a host pass never sees it.
#
# The server's test fixture plugins (packages/server/tests/fixtures*) ship
# only as WASM, which `scripts/build-plugins.sh` builds, so they get the WASM
# pass alone.
#
# Deliberately invoked from the repo root and never `cd`s into a plugin
# directory: some plugin crates carry a local `.cargo/config.toml` that pins
# a non-host default `[build] target` (e.g. standard-languages defaults to
# wasm32-wasip1). Cargo's config-file discovery walks up from the *current
# working directory*, not from `--manifest-path`, so running from the repo
# root (which carries no such override) keeps the host pass on the host, and
# the WASM pass names its target explicitly.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PLUGINS_DIR="${ROOT_DIR}/plugins"
FIXTURES_DIR="${ROOT_DIR}/packages/server/tests"
WASM_TARGET="wasm32-wasip1"
cd "${ROOT_DIR}"
# One target directory for every plugin crate (see scripts/build-plugins.sh),
# so the dependencies they share build once.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT_DIR}/target/plugins}"

if [[ ! -d "${PLUGINS_DIR}" ]]; then
  echo "no plugins directory at ${PLUGINS_DIR}" >&2
  exit 1
fi

# Whether the crate at manifest $1 ships as a WASM backend.
ships_as_wasm() {
  local manifest_toml
  manifest_toml="$(dirname "$1")/plugin.toml"
  [[ -f "${manifest_toml}" ]] && grep -Eq '^\[server\][[:space:]]*$' "${manifest_toml}"
}

lint_wasm() {
  echo "==> Linting ${1#"${ROOT_DIR}/"} for ${WASM_TARGET}"
  cargo clippy --manifest-path "$1" --target "${WASM_TARGET}" --locked -- -D warnings
}

linted_any=0

while IFS= read -r -d '' manifest; do
  echo "==> Linting ${manifest#"${ROOT_DIR}/"} for the host"
  cargo clippy --manifest-path "${manifest}" --all-targets --locked -- -D warnings
  if ships_as_wasm "${manifest}"; then
    lint_wasm "${manifest}"
  fi
  linted_any=1
done < <(find "${PLUGINS_DIR}" -name target -prune -o -name Cargo.toml -print0 | sort -z)

while IFS= read -r -d '' manifest; do
  if ships_as_wasm "${manifest}"; then
    lint_wasm "${manifest}"
  fi
done < <(find "${FIXTURES_DIR}"/fixtures* -name target -prune -o -name Cargo.toml -print0 | sort -z)

if [[ ${linted_any} -eq 0 ]]; then
  echo "error: no plugin crates were found to lint" >&2
  exit 1
fi
