#!/usr/bin/env bash
# Type-checks every plugin frontend package under `plugins/` with `tsc --noEmit`.
#
# `pnpm typecheck` at the repo root only runs `pnpm --filter @broccoli/web
# typecheck` -- no plugin frontend is type-checked by anything. `pnpm build`
# in those packages runs `tsdown`/`vite`, which transpile without a full
# type-check, so a plugin frontend can carry real type errors indefinitely
# and still build and ship.
#
# See scripts/test-plugins.sh for the discovery rationale this mirrors:
# targets are found by searching for `tsconfig.json` manifests (depth-agnostic,
# `node_modules/` and `target/` pruned) rather than a hardcoded list or a
# one-level glob, so a newly added plugin frontend is picked up automatically.
#
# Root plugin frontend packages are intentionally not pnpm workspace members
# (see pnpm-workspace.yaml), so each is invoked via `pnpm --dir <path>` rather
# than `pnpm --filter`.
#
# Assumes the shared web SDK's `dist` has already been built (plugin
# frontends resolve `@broccoli/web-sdk`'s subpaths through its published
# `exports` map, which points at `dist`, not `src`) -- e.g. via
# `pnpm --filter @broccoli/web-sdk build`, as the CI `frontend` job's
# "Build web SDK" step already does before this script's step.
#
# Installs each plugin frontend's own dependencies first. They are not pnpm
# workspace members, so the root `pnpm install` never installs them, and no
# earlier step in CI's `frontend` job does either. Without this the gate only
# ever passed on machines where `build-plugins.sh` had already installed them,
# and failed in CI with `tsc: not found`. Mirrors the dev CLI's frontend
# install (`pnpm install --ignore-workspace`), plus `--frozen-lockfile` so a
# stale per-plugin lockfile fails here rather than drifting silently.
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
  plugin_dir="$(dirname "${manifest}")"
  plugin_name="${plugin_dir#"${ROOT_DIR}/"}"
  echo "==> Type checking plugin frontend: ${plugin_name}"
  pnpm --dir "${plugin_dir}" install --ignore-workspace --frozen-lockfile --reporter=silent
  pnpm --dir "${plugin_dir}" exec tsc --noEmit -p tsconfig.json
  checked_any=1
done < <(
  find "${PLUGINS_DIR}" \( -name node_modules -o -name target \) -prune -o \
    -name tsconfig.json -print0 | sort -z
)

if [[ ${checked_any} -eq 0 ]]; then
  echo "error: no plugin frontend packages were found to type check" >&2
  exit 1
fi
