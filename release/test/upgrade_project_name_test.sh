#!/usr/bin/env bash
# An env file from before COMPOSE_PROJECT_NAME existed takes its project name
# from the bundle directory. Copied into a new release's directory, it must
# not start infra on new, empty volumes while the old database volume sits
# under the previous directory's project: install.sh refuses until the name
# is pinned. Needs a Docker daemon (creates one labelled, empty volume).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
rel="$(cd "$here/.." && pwd)"

if ! docker volume ls >/dev/null 2>&1; then
  echo "SKIP: needs a Docker daemon"
  exit 0
fi

T="$(mktemp -d)"
old_project="broccoli-upgrade-test-$$-v020"
volume="${old_project}_postgres_data"
cleanup() { docker volume rm "$volume" >/dev/null 2>&1 || true; rm -rf "$T"; }
trap cleanup EXIT
fail() { echo "FAIL: $*"; exit 1; }

new_dir="$T/broccoli-upgrade-test-$$-v0.3.0"
cp -r "$rel/." "$new_dir"
cd "$new_dir"
run() { BROCCOLI_DRY_RUN=1 BROCCOLI_SKIP_TIME_CHECK=1 BROCCOLI_INFRA_HOST=10.0.0.10 ./install.sh infra </dev/null >"$T/out.log" 2>&1; }

run || { cat "$T/out.log"; fail "first infra dry run"; }
# Make it an env file from an earlier release: no project name.
grep -v '^COMPOSE_PROJECT_NAME=' .env.infra > .env.infra.old && mv .env.infra.old .env.infra
docker volume create --label "com.docker.compose.project=$old_project" \
  --label com.docker.compose.volume=postgres_data "$volume" >/dev/null

if run; then fail "infra would have started on empty volumes beside $volume"; fi
grep -q "$old_project" "$T/out.log" || { cat "$T/out.log"; fail "the refusal does not name the project holding the data"; }

printf "COMPOSE_PROJECT_NAME='%s'\n" "$old_project" >> .env.infra
run || { cat "$T/out.log"; fail "a pinned project name was refused"; }

# A legacy env file re-run in place (the directory still matches) is fine.
grep -v '^COMPOSE_PROJECT_NAME=' .env.infra > .env.infra.old && mv .env.infra.old .env.infra
cd "$T" && mv "$new_dir" "$T/$old_project" && cd "$T/$old_project"
run || { cat "$T/out.log"; fail "an in-place re-run of a legacy env file was refused"; }
echo "PASS: unpinned project names cannot strand an upgrade on empty volumes"
