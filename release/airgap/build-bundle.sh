#!/usr/bin/env bash
# Assemble a single self-contained broccoli air-gap bundle on a networked
# staging box. STAGING-SIDE: may pull/build/docker-save. The produced tree
# is carried to the air-gapped LAN and installed with zero network.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
# shellcheck source=/dev/null
source "$here/lib/manifest.sh"
# shellcheck source=/dev/null
source "$here/lib/envgen.sh"
# shellcheck source=/dev/null
source "$here/lib/runtime.sh"

usage() {
  echo "Usage: build-bundle.sh --version V [--output DIR] [--lan-host H] [--tar] [--skip-images]"
}

VERSION="" OUTPUT="./dist" LAN_HOST="" TAR=0 SKIP_IMAGES=0
while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --output) OUTPUT="$2"; shift 2 ;;
    --lan-host) LAN_HOST="$2"; shift 2 ;;
    --tar) TAR=1; shift ;;
    --skip-images) SKIP_IMAGES=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown arg: $1" >&2; usage; exit 2 ;;
  esac
done
[ -n "$VERSION" ] || { echo "--version is required" >&2; usage; exit 2; }
case "$VERSION" in
  *[!A-Za-z0-9._-]*) echo "--version must match [A-Za-z0-9._-] (got: $VERSION)" >&2; exit 2 ;;
esac

# Provenance stamped into the images and bundle.json: the commit and the
# moment this bundle was built.
git_sha="$(git -C "$repo" rev-parse HEAD 2>/dev/null || echo unknown)"
created="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

B="$OUTPUT/broccoli-airgap-$VERSION"
rm -rf "$B"
mkdir -p "$B"/{images,compose,cli,ca,caddy,trust-ca,lib,native}

# 1. CA — private keys NEVER enter the client-distributed (manifested) tree.
#    Only the public root.crt ships to clients. root.key (and the leaf's
#    server.key) live in a SEPARATE, unmanifested server-only sidecar that the
#    operator delivers to the server host ALONE. See docs/airgap-deployment.md §7.
SRV="$OUTPUT/broccoli-airgap-$VERSION.server-secret"
rm -rf "$SRV"; mkdir -p "$SRV"; chmod 700 "$SRV"
bash "$here/ca/mint-ca.sh" --out "$SRV"
cp "$SRV/root.crt" "$B/ca/root.crt"
if [ -n "$LAN_HOST" ]; then
  bash "$here/ca/issue-leaf.sh" --ca-dir "$SRV" --host "$LAN_HOST" --out "$SRV"
fi

# 1b. Cluster-secret sidecar: machine secrets shared by server + workers.
#     Sibling of the bundle, UNMANIFESTED, delivered to the server AND every
#     worker host — never to contestants. JWT is NOT here (worker needs none;
#     envgen mints it into .env.server). See docs/airgap-deployment.md.
CLS="$OUTPUT/broccoli-airgap-$VERSION.cluster-secret"
rm -rf "$CLS"; mkdir -p "$CLS"; chmod 700 "$CLS"
{
  printf 'POSTGRES_PASSWORD=%s\n' "$(envgen_secret 24)"
  printf 'BROCCOLI_DB_PASSWORD=%s\n' "$(envgen_secret 24)"
  printf 'REDIS_PASSWORD=%s\n' "$(envgen_secret 24)"
  printf 'BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY=%s\n' "$(envgen_secret 18)"
  printf 'BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY=%s\n' "$(envgen_secret 24)"
  [ -n "$LAN_HOST" ] && printf 'BROCCOLI_SERVER_HOST=%s\n' "$LAN_HOST"
} > "$CLS/cluster-secrets.env"
chmod 600 "$CLS/cluster-secrets.env"

# 2. Target-side scripts + Caddyfile + trust helpers + installer libs
cp "$here/load-bundle.sh" "$here/install.sh" "$here/setup.sh" "$B/"
cp "$here"/lib/*.sh "$B/lib/"
cp "$repo"/release/lib/*.sh "$B/lib/"
cp "$repo/release/enable-plugin.sh" "$B/enable-plugin.sh"
cp "$here/caddy/Caddyfile.airgap" "$B/caddy/Caddyfile.airgap"
cp "$here"/ca/issue-leaf.sh "$here"/ca/sign.sh "$B/ca/"
cp "$here"/trust-ca/* "$B/trust-ca/"
cp "$repo/release/native/live-boot-preflight.sh" "$B/native/live-boot-preflight.sh"
chmod +x "$B/native/live-boot-preflight.sh"

# 3. Compose templates + env examples (reuse release/, do not fork)
cp "$repo/release/docker-compose.server.yaml.template" \
   "$repo/release/docker-compose.infra.yaml.template" \
   "$repo/release/docker-compose.worker.yaml.template" \
   "$repo/release/docker-compose.server-airgap.yaml.template" \
   "$repo/release/docker-compose.gateway-airgap.yaml.template" "$B/compose/"
cp "$repo/release/.env.server.example" "$repo/release/.env.infra.example" \
   "$repo/release/.env.worker.example" "$B/compose/"
# Rewrite the STAGED examples' image refs to the LOCAL versioned tags so the
# target resolves them under `--pull never` (never touch the repo originals).
env_set "$B/compose/.env.server.example" BROCCOLI_SERVER_IMAGE "broccoli-server:$VERSION"
env_set "$B/compose/.env.worker.example" BROCCOLI_WORKER_IMAGE "broccoli-worker:$VERSION"

# 4. Images + CLI (heavy; skipped for CI structural tests)
if [ "$SKIP_IMAGES" = "0" ]; then
  ENGINE="${BROCCOLI_ENGINE:-$(runtime_engine)}"
  [ -n "$ENGINE" ] || { echo "no docker/podman found for image build (install one, or pass --skip-images)" >&2; exit 2; }

  # Optional build-arg passthrough for staging boxes that cannot reach the image
  # defaults. This air-gap feature targets mirror-fronted LANs (e.g. Chinese
  # infra) whose staging box has NO route to gcr.io/distroless (the default
  # server runtime base) and only a throttled route to static.rust-lang.org
  # (the toolchain). Both Dockerfiles already carry RUNTIME_IMAGE / USE_CN_MIRRORS
  # levers; surface them here so the documented staging entrypoint can flip them
  # WITHOUT weakening the shipped defaults — unset means distroless runtime +
  # upstream toolchain, exactly as before. USE_CN_MIRRORS applies to both images;
  # RUNTIME_IMAGE is server-only (the worker bases are already mirror-agnostic).
  mirror_build_args=()
  [ -n "${BROCCOLI_USE_CN_MIRRORS:-}" ] && \
    mirror_build_args+=(--build-arg "USE_CN_MIRRORS=$BROCCOLI_USE_CN_MIRRORS")
  server_runtime_arg=()
  [ -n "${BROCCOLI_RUNTIME_IMAGE:-}" ] && \
    server_runtime_arg=(--build-arg "RUNTIME_IMAGE=$BROCCOLI_RUNTIME_IMAGE")

  # Stamp provenance into both images' OCI labels
  # (org.opencontainers.image.{version,revision,created}); without these every
  # shipped image reports the Dockerfiles' "dev"/"unknown". Air-gapped targets
  # have no registry to query, so
  # `docker inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}'`
  # on the loaded image is the ONLY offline way to tell which commit a site
  # runs (e.g. whether it includes a given fix) or that an upgrade swapped it.
  # Passed as --label, not --build-arg: a build arg in scope of the worker's
  # apt RUNs that changes every build (CREATED is a timestamp) would rebuild
  # every toolchain stage each time. A label touches only the final image.
  provenance_labels=(--label "org.opencontainers.image.version=$VERSION"
    --label "org.opencontainers.image.revision=$git_sha"
    --label "org.opencontainers.image.created=$created")

  # broccoli images — built from the repo. Frontend is baked fresh into the
  # server image (verify served bundle behaviorally, not by mtime).
  "$ENGINE" build -f "$repo/Dockerfile.server" \
    "${mirror_build_args[@]}" "${server_runtime_arg[@]}" "${provenance_labels[@]}" \
    -t "broccoli-server:$VERSION" "$repo"
  "$ENGINE" build -f "$repo/Dockerfile.worker" --target runtime-full \
    "${mirror_build_args[@]}" "${provenance_labels[@]}" \
    -t "broccoli-worker:$VERSION" "$repo"

  # Default plugins for the bind-mount source. The server compose template
  # mounts ./plugins:/plugins:ro, which OVERLAYS the image-baked
  # /plugins; if the bundle omits that source dir the overlay is empty and the
  # server boots with an empty plugin registry (discover_plugins scans the dir at
  # startup — no evaluators/checkers means nothing judges). Copy the built set
  # straight out of the server image we just built: the single source of truth,
  # already pruned by .dockerignore (no target/ detritus) and carrying the built
  # .wasm + frontend dist that are gitignored on disk. Staged into the manifested
  # tree below, so bundle integrity covers the plugin code too.
  # The image already split them by [bundle] mode (release/lib/plugins.sh):
  # optional plugins ride along in plugins-available/ for enable-plugin.sh.
  rm -rf "$B/compose/plugins" "$B/compose/plugins-available"
  mkdir -p "$B/compose/plugins" "$B/compose/plugins-available"
  pcid="$("$ENGINE" create "broccoli-server:$VERSION")"
  "$ENGINE" cp "$pcid:/plugins/." "$B/compose/plugins/"
  "$ENGINE" cp "$pcid:/plugins-available/." "$B/compose/plugins-available/"
  "$ENGINE" rm "$pcid" >/dev/null

  # third-party image tags — single-sourced (DRY) from the staged examples/template
  ex="$B/compose/.env.infra.example"
  pg_img="$(env_get "$ex" POSTGRES_IMAGE)"
  redis_img="$(env_get "$ex" REDIS_IMAGE)"
  swfs_img="$(env_get "$ex" SEAWEEDFS_IMAGE)"
  # CADDY_IMAGE has no .env row; its sole source of truth is the
  # ${CADDY_IMAGE:-...} default in the gateway template. Parse it defensively:
  # grep -m1 stops at the first match (no `head` closing the pipe -> SIGPIPE ->
  # pipefail abort), and `|| true` keeps a no-match from aborting under set -e so
  # the :- fallback supplies the same literal the template ships.
  caddy_img="$(grep -oE -m1 'CADDY_IMAGE:-[^}]+' "$B/compose/docker-compose.gateway-airgap.yaml.template" | cut -d- -f2- || true)"
  caddy_img="${caddy_img:-caddy:2-alpine}"
  for img in "$pg_img" "$redis_img" "$swfs_img" "$caddy_img"; do
    [ -n "$img" ] || { echo "could not resolve a third-party image tag" >&2; exit 1; }
    # Prefer a fresh pull, but do NOT hard-abort when the registry is unreachable
    # IF the image is already in the local store. This air-gap feature targets
    # mirror-fronted / intermittently-connected staging boxes where docker.io is
    # often blocked (empty daemon registry-mirrors) and the operator has
    # pre-seeded the third-party images — or a prior build-bundle run left them
    # local. Falling back to the local copy keeps the bundle assemblable offline;
    # an image that is neither pullable nor present still aborts loudly.
    if ! "$ENGINE" pull "$img"; then
      if "$ENGINE" image inspect "$img" >/dev/null 2>&1; then
        echo "WARN: pull failed for $img; using the copy already in the local image store" >&2
      else
        echo "ERROR: cannot pull $img and it is not in the local image store — pre-seed it (docker pull via your mirror) or fix registry access" >&2
        exit 1
      fi
    fi
  done

  # save each image to its own tar (independent docker-load + per-image integrity)
  "$ENGINE" save "broccoli-server:$VERSION" > "$B/images/server.tar"
  "$ENGINE" save "broccoli-worker:$VERSION" > "$B/images/worker.tar"
  "$ENGINE" save "$pg_img"    > "$B/images/postgres.tar"
  "$ENGINE" save "$redis_img" > "$B/images/redis.tar"
  "$ENGINE" save "$swfs_img"  > "$B/images/seaweedfs.tar"
  "$ENGINE" save "$caddy_img" > "$B/images/caddy.tar"

  # contestant CLI — static musl (per noi-parity-worker-image)
  ( cd "$repo" && cargo build -p broccoli-contestant-cli --profile release-cli \
      --target x86_64-unknown-linux-musl )
  cp "$repo/target/x86_64-unknown-linux-musl/release-cli/broccoli" "$B/cli/broccoli"
  chmod 0755 "$B/cli/broccoli"
fi

# 5. Provenance + integrity
cat > "$B/bundle.json" <<JSON
{
  "version": "$VERSION",
  "git_sha": "$git_sha",
  "created": "$created",
  "roles": ["server", "gateway", "worker", "contestant"]
}
JSON
manifest_generate "$B"
# Ship-clean assertion (defense-in-depth): the client-distributed tree must never
# carry on-host env config — those basenames are manifest-excluded, so one leaked
# in here (a dirty release/ dir, a future cp bug) would ride every bundle past
# integrity verification undetected. Fail the build loudly rather than ship it.
manifest_no_hostenv "$B" \
  || { echo "BUILD ABORT: assembled bundle carries on-host env config (see above) — refusing to ship" >&2; exit 1; }
echo "assembled bundle: $B"
echo "SERVER-ONLY SECRETS: $SRV"
echo "  contains CA/leaf private keys (root.key$([ -n "$LAN_HOST" ] && echo ', server.key')) — deliver ONLY to the server host; never to workers/contestants"
echo "CLUSTER SECRETS: $CLS"
echo "  contains DB/redis/S3 passwords shared by server+workers — deliver to the server AND every worker host; never to contestants"

if [ "$TAR" = "1" ]; then
  ( cd "$OUTPUT" && tar -caf "broccoli-airgap-$VERSION.tar.zst" "broccoli-airgap-$VERSION" )
  echo "tarball: $OUTPUT/broccoli-airgap-$VERSION.tar.zst"
fi
