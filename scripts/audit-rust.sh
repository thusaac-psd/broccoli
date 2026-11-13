#!/usr/bin/env bash
# Runs `cargo audit` over every committed Cargo.lock that drives a build: the
# root workspace, each plugin crate, the server test fixtures and the
# examples. Lockfiles are discovered with `git ls-files`, so a new plugin is
# covered without editing this script.
#
# Runs from the repository root so `.cargo/audit.toml` (the documented ignore
# list) applies to every lockfile. `third_party/` is skipped: the vendored
# crates there build through the root lockfile, and their own lockfiles are
# never resolved.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT_DIR}"

status=0
audited_any=0

while IFS= read -r -d '' lockfile; do
  echo "==> cargo audit: ${lockfile}"
  if ! cargo audit --file "${lockfile}"; then
    status=1
  fi
  audited_any=1
done < <(git ls-files -z -- '*Cargo.lock' ':!:third_party/**')

if [[ ${audited_any} -eq 0 ]]; then
  echo "error: no Cargo.lock files were found to audit" >&2
  exit 1
fi

exit "${status}"
