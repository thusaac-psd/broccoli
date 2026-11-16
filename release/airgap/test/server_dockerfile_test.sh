#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
df="$here/../../../Dockerfile.server"
[ -f "$df" ] || { echo "FAIL: Dockerfile.server missing: $df"; exit 1; }

# The server image must give plugins a writable HOME: the runtime stage runs as the
# distroless nonroot uid 65532, whose HOME (/home/nonroot) is NOT writable by that
# user in the base image. wasmtime (via extism) derives its plugin compilation
# cache from $HOME and fails plugin init() with "failed to create cache directory"
# when it cannot mkdir under $HOME. Every evaluator/checker/hook plugin then fails
# to register: the server boots "healthy" but the evaluator registry is empty, so
# no problem can be created ("problem_type must be one of:" empty) and nothing
# judges. The image must bake a writable, uid-owned HOME and point the process at
# it. Assert all three legs so a base bump or refactor cannot silently bring
# back the empty-registry failure.

# 1. The shell-bearing builder stage creates the home dir (distroless can't mkdir).
grep -Eq '^[[:space:]]*RUN[[:space:]]+mkdir[[:space:]]+-p[[:space:]]+/out/home/nonroot' "$df" \
  || { echo "FAIL: Dockerfile.server does not create /out/home/nonroot in a builder stage"; exit 1; }

# 2. The runtime stage copies it in owned by 65532 (a writable HOME, not the base's
#    root-owned one).
grep -Eq '^[[:space:]]*COPY[[:space:]].*--chown=65532:65532[[:space:]].*/out/home/nonroot[[:space:]]+/home/nonroot' "$df" \
  || { echo "FAIL: Dockerfile.server does not COPY --chown=65532:65532 a writable /home/nonroot into runtime"; exit 1; }

# 3. HOME is set explicitly (not left to the base default) so wasmtime resolves
#    $HOME/.cache to the writable dir regardless of the base image.
grep -Eq '^[[:space:]]*ENV[[:space:]]+HOME=/home/nonroot([[:space:]]|\\|$)' "$df" \
  || { echo "FAIL: Dockerfile.server does not set ENV HOME=/home/nonroot (wasmtime cache would fall back to an unwritable path)"; exit 1; }

# 4. CARGO_BUILD_JOBS must carry a non-empty default. The rust-builder does
#    `ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}`; an UNSET build-arg expands to an
#    empty string, and cargo rejects "" hard ("could not parse ``"). build-bundle.sh
#    (the shipped air-gap staging entrypoint) passes NO --build-arg, so an empty
#    default is a release blocker — the bundle's server image cannot build. Assert
#    the ARG defaults to a value cargo accepts (`default` or a number).
grep -Eq '^[[:space:]]*ARG[[:space:]]+CARGO_BUILD_JOBS=(default|[0-9]+)([[:space:]]|$)' "$df" \
  || { echo "FAIL: Dockerfile.server ARG CARGO_BUILD_JOBS has no non-empty default; an unset build-arg -> empty string -> cargo panics ('could not parse \`\`')"; exit 1; }

echo "PASS: server image bakes a writable uid-65532 HOME so plugins can instantiate/register"
