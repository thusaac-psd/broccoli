#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
bb="$here/../build-bundle.sh"
[ -f "$bb" ] || { echo "FAIL: build-bundle.sh missing"; exit 1; }
bash -n "$bb" || { echo "FAIL: build-bundle.sh does not parse"; exit 1; }

# structural: mints CA and writes manifest + bundle.json
grep -q 'mint-ca.sh'      "$bb" || { echo "FAIL: does not mint CA"; exit 1; }
grep -q 'manifest_generate' "$bb" || { echo "FAIL: does not generate manifest"; exit 1; }
grep -q 'bundle.json'     "$bb" || { echo "FAIL: does not write bundle.json"; exit 1; }

# --skip-images assembles a tree with no docker (CI-safe)
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
bash "$bb" --version testv --output "$T" --skip-images >/dev/null
b="$T/broccoli-airgap-testv"
for p in bundle.json manifest.sha256 ca/root.crt caddy/Caddyfile.airgap \
         compose/docker-compose.gateway-airgap.yaml.template \
         load-bundle.sh install.sh setup.sh trust-ca/linux.sh compose \
         lib/manifest.sh lib/runtime.sh lib/answers.sh lib/envgen.sh \
         lib/preflight.sh native/live-boot-preflight.sh; do
  [ -e "$b/$p" ] || { echo "FAIL: bundle missing $p"; exit 1; }
done
[ -x "$b/native/live-boot-preflight.sh" ] || { echo "FAIL: staged preflight not executable"; exit 1; }
[ -x "$b/setup.sh" ] || { echo "FAIL: staged setup.sh not executable"; exit 1; }
# worker deploy files must be staged
for p in compose/docker-compose.worker.yaml.template compose/.env.worker.example; do
  [ -e "$b/$p" ] || { echo "FAIL: bundle missing $p"; exit 1; }
done
# staged env examples carry LOCAL versioned image tags so target `--pull never` resolves
grep -qx "BROCCOLI_SERVER_IMAGE='broccoli-server:testv'" "$b/compose/.env.server.example" \
  || { echo "FAIL: .env.server.example image tag not rewritten to broccoli-server:testv"; exit 1; }
grep -qx "BROCCOLI_WORKER_IMAGE='broccoli-worker:testv'" "$b/compose/.env.worker.example" \
  || { echo "FAIL: .env.worker.example image tag not rewritten to broccoli-worker:testv"; exit 1; }
# manifest actually verifies
# shellcheck source=/dev/null
source "$here/../lib/manifest.sh"; manifest_verify "$b" >/dev/null \
  || { echo "FAIL: assembled bundle fails its own manifest"; exit 1; }
# ship-clean: build-bundle must assert the assembled tree carries NO on-host env
# config (those basenames are manifest-excluded, so a leaked one would ride every
# bundle undetected) — and the assembled tree must actually be clean.
grep -q 'manifest_no_hostenv' "$bb" \
  || { echo "FAIL: build-bundle must assert ship-clean via manifest_no_hostenv"; exit 1; }
manifest_no_hostenv "$b" >/dev/null 2>&1 \
  || { echo "FAIL: assembled bundle carries on-host env config"; exit 1; }
# security: NO private key ever enters the client-distributed (manifested) tree.
for k in ca/root.key ca/server.key; do
  [ ! -e "$b/$k" ] || { echo "FAIL: private key leaked into client tree: $k"; exit 1; }
done
grep -qE '(^| )\./ca/(root|server)\.key$' "$b/manifest.sha256" \
  && { echo "FAIL: manifest lists a private key"; exit 1; } || true
# the CA/leaf private keys live in the server-only sidecar instead
[ -f "$T/broccoli-airgap-testv.server-secret/root.key" ] \
  || { echo "FAIL: server-secret sidecar missing root.key"; exit 1; }

# cluster-secret sidecar: sibling, present, has the shared machine-secret keys, NOT manifested
cls="$T/broccoli-airgap-testv.cluster-secret/cluster-secrets.env"
[ -f "$cls" ] || { echo "FAIL: cluster-secret sidecar missing cluster-secrets.env"; exit 1; }
for k in POSTGRES_PASSWORD REDIS_PASSWORD \
         BROCCOLI__STORAGE__OBJECT_STORAGE__ACCESS_KEY \
         BROCCOLI__STORAGE__OBJECT_STORAGE__SECRET_KEY; do
  grep -qE "^${k}=" "$cls" || { echo "FAIL: cluster-secret missing $k"; exit 1; }
done
# no lan-host on this build -> no server host baked
grep -qE '^BROCCOLI_SERVER_HOST=' "$cls" && { echo "FAIL: server host baked without --lan-host"; exit 1; } || true
# leak guard: cluster secrets never appear in the manifested tree
grep -q 'cluster-secret' "$b/manifest.sha256" && { echo "FAIL: cluster-secret path leaked into manifest"; exit 1; } || true

# second build WITH --lan-host bakes BROCCOLI_SERVER_HOST
bash "$bb" --version testv2 --output "$T" --lan-host contest.lan --skip-images >/dev/null
cls2="$T/broccoli-airgap-testv2.cluster-secret/cluster-secrets.env"
grep -qx 'BROCCOLI_SERVER_HOST=contest.lan' "$cls2" \
  || { echo "FAIL: --lan-host not baked into cluster-secret as BROCCOLI_SERVER_HOST"; exit 1; }

# --- caddy image tag parse must be crash-proof under `set -euo pipefail`. The
#     tag's only source of truth is the ${CADDY_IMAGE:-...} default in the gateway
#     template; the parse must (a) use grep -m1 (no `head` closing the pipe early
#     -> SIGPIPE -> pipefail abort even on a match) and (b) tolerate a no-match
#     without aborting, so the :- fallback can supply the shipped literal. ---
grep -qE "grep -oE -m1 'CADDY_IMAGE" "$bb" \
  || { echo "FAIL: caddy tag parse must use 'grep -oE -m1' (head|pipefail SIGPIPE hazard)"; exit 1; }
grep -qE "CADDY_IMAGE:-\[\^}\]\+.*\|\| true" "$bb" \
  || { echo "FAIL: caddy tag parse must guard no-match with '|| true' (else set -e aborts before the fallback)"; exit 1; }
# behavioral: replicate the parse contract — a template with NO CADDY_IMAGE line
# must NOT abort and must fall through to the shipped default.
gw_real="$here/../../docker-compose.gateway-airgap.yaml.template"
tag_real="$( set -euo pipefail; c="$(grep -oE -m1 'CADDY_IMAGE:-[^}]+' "$gw_real" | cut -d- -f2- || true)"; echo "${c:-caddy:2-alpine}" )"
[ "$tag_real" = 'caddy:2-alpine' ] || { echo "FAIL: caddy parse on the real template got '$tag_real', want caddy:2-alpine"; exit 1; }
nomatch="$(mktemp)"; printf 'services:\n  gateway:\n    image: caddy\n' > "$nomatch"
tag_none="$( set -euo pipefail; c="$(grep -oE -m1 'CADDY_IMAGE:-[^}]+' "$nomatch" | cut -d- -f2- || true)"; echo "${c:-caddy:2-alpine}" )"
rc=$?; rm -f "$nomatch"
[ "$rc" = 0 ] || { echo "FAIL: caddy parse aborted (rc=$rc) on a template with no CADDY_IMAGE line"; exit 1; }
[ "$tag_none" = 'caddy:2-alpine' ] || { echo "FAIL: caddy parse no-match fallback got '$tag_none', want caddy:2-alpine"; exit 1; }

# --- optional build-arg passthrough: a mirror-fronted staging box (the Chinese
#     infra this air-gap feature targets) has no route to gcr.io/distroless nor a
#     fast one to static.rust-lang.org. build-bundle must surface the Dockerfiles'
#     RUNTIME_IMAGE / USE_CN_MIRRORS levers via env WITHOUT weakening the shipped
#     defaults (unset => distroless + upstream, no --build-arg emitted at all). ---
grep -qE 'BROCCOLI_USE_CN_MIRRORS.*--build-arg|mirror_build_args\+=\(--build-arg "USE_CN_MIRRORS=' "$bb" \
  || { echo "FAIL: build-bundle does not pass BROCCOLI_USE_CN_MIRRORS through as --build-arg USE_CN_MIRRORS"; exit 1; }
grep -qE 'server_runtime_arg=\(--build-arg "RUNTIME_IMAGE=\$BROCCOLI_RUNTIME_IMAGE"' "$bb" \
  || { echo "FAIL: build-bundle does not pass BROCCOLI_RUNTIME_IMAGE through as --build-arg RUNTIME_IMAGE (server)"; exit 1; }
# the mirror args must reach BOTH image builds; the runtime override is server-only
# (worker bases are mirror-agnostic and have no RUNTIME_IMAGE arg to consume).
awk '/Dockerfile.server/{s=1} s&&/mirror_build_args\[@\]/{sm=1} s&&/server_runtime_arg\[@\]/{sr=1} /-t "broccoli-server/{s=0}
     /Dockerfile.worker/{w=1} w&&/mirror_build_args\[@\]/{wm=1} /-t "broccoli-worker/{w=0}
     END{ if(!sm){print "MISS server mirror"; exit 1}
          if(!sr){print "MISS server runtime"; exit 1}
          if(!wm){print "MISS worker mirror"; exit 1} }' "$bb" \
  || { echo "FAIL: build-arg passthrough not wired to the right image build(s)"; exit 1; }
# the worker build must NOT receive the server-only RUNTIME_IMAGE override.
awk '/Dockerfile.worker/{w=1} w&&/server_runtime_arg\[@\]/{print "worker got runtime arg"; exit 1} /-t "broccoli-worker/{w=0}' "$bb" \
  || { echo "FAIL: RUNTIME_IMAGE override leaked into the worker build"; exit 1; }
# behavioral: the array-building idiom must be safe under `set -u` when the env
# vars are UNSET (empty array expansion must not error and must add NO elements)
# AND emit exactly the right flags — each a distinct token — when set.
unset_cnt="$( set -u
  mirror_build_args=(); server_runtime_arg=()
  [ -n "${BROCCOLI_USE_CN_MIRRORS:-}" ] && mirror_build_args+=(--build-arg "USE_CN_MIRRORS=$BROCCOLI_USE_CN_MIRRORS")
  [ -n "${BROCCOLI_RUNTIME_IMAGE:-}" ] && server_runtime_arg=(--build-arg "RUNTIME_IMAGE=$BROCCOLI_RUNTIME_IMAGE")
  all=( "${mirror_build_args[@]}" "${server_runtime_arg[@]}" ); echo "${#all[@]}" )"
[ "$unset_cnt" = "0" ] || { echo "FAIL: unset env must emit NO build-args (got element count: $unset_cnt)"; exit 1; }
set_out="$( set -u
  export BROCCOLI_USE_CN_MIRRORS=true BROCCOLI_RUNTIME_IMAGE=debian:bookworm-slim
  mirror_build_args=(); server_runtime_arg=()
  [ -n "${BROCCOLI_USE_CN_MIRRORS:-}" ] && mirror_build_args+=(--build-arg "USE_CN_MIRRORS=$BROCCOLI_USE_CN_MIRRORS")
  [ -n "${BROCCOLI_RUNTIME_IMAGE:-}" ] && server_runtime_arg=(--build-arg "RUNTIME_IMAGE=$BROCCOLI_RUNTIME_IMAGE")
  all=( "${mirror_build_args[@]}" "${server_runtime_arg[@]}" ); printf '<%s>' "${all[@]}" )"
[ "$set_out" = "<--build-arg><USE_CN_MIRRORS=true><--build-arg><RUNTIME_IMAGE=debian:bookworm-slim>" ] \
  || { echo "FAIL: set env must emit both --build-args as distinct tokens (got: $set_out)"; exit 1; }

# --- the bundle VERSION must reach BOTH images' OCI version label. Both
#     Dockerfiles carry `ARG VERSION=dev` -> org.opencontainers.image.version;
#     without an emitted `--build-arg VERSION` every shipped image reports "dev"
#     regardless of the bundle version. Air-gapped targets have no registry to
#     query, so the loaded image's version label is the ONLY offline way to tell
#     one bundle's images from the next's (e.g. confirming an in-place upgrade
#     actually swapped the image). Unlike the mirror levers this is
#     unconditional — every build stamps it. ---
grep -qE 'version_build_arg=\(--build-arg "VERSION=\$VERSION"\)' "$bb" \
  || { echo "FAIL: build-bundle must stamp the bundle VERSION via --build-arg VERSION (image OCI label is the only offline provenance signal)"; exit 1; }
awk '/Dockerfile.server/{s=1} s&&/version_build_arg\[@\]/{sv=1} /-t "broccoli-server/{s=0}
     /Dockerfile.worker/{w=1} w&&/version_build_arg\[@\]/{wv=1} /-t "broccoli-worker/{w=0}
     END{ if(!sv){print "MISS server version"; exit 1}
          if(!wv){print "MISS worker version"; exit 1} }' "$bb" \
  || { echo "FAIL: --build-arg VERSION not wired into both image builds"; exit 1; }

# --- third-party image pull must tolerate a blocked registry when the image is
#     ALREADY in the local store. This air-gap feature targets mirror-fronted /
#     intermittently-connected staging boxes where docker.io is often blocked
#     (empty daemon registry-mirrors) and the operator has pre-seeded the pg/
#     redis/seaweedfs/caddy images (or a prior build-bundle run left them local).
#     An UNCONDITIONAL `docker pull` aborts under `set -e` on such a box even
#     though the bundle could be assembled from the local copy. Require: pull,
#     and on failure fall back to the local image store; abort only if the image
#     is truly absent. (Invisible to --skip-images: the loop is image-gated.) ---
grep -qE 'if ! "\$ENGINE" pull "\$img"' "$bb" \
  || { echo "FAIL: third-party pull must be failure-tolerant ('if ! \$ENGINE pull'), not an unconditional abort"; exit 1; }
grep -qE '"\$ENGINE" image inspect "\$img"' "$bb" \
  || { echo "FAIL: third-party pull must fall back to the local image store ('image inspect \$img') on pull failure"; exit 1; }
# behavioral: replicate the fallback contract with a stub engine (a function
# named in $ENGINE, invoked exactly as the script does — "$ENGINE" pull / image).
#   pull fails + image present  -> loop continues (rc 0)
#   pull fails + image absent   -> loop aborts    (rc 1)
eng_present() { case "$1" in pull) return 1 ;; image) return 0 ;; *) return 0 ;; esac; }
eng_absent()  { case "$1" in pull) return 1 ;; image) return 1 ;; *) return 0 ;; esac; }
run_pull_body() { # $1 = engine fn name
  set -euo pipefail
  local ENGINE="$1" img="postgres:18-alpine"
  if ! "$ENGINE" pull "$img"; then
    if "$ENGINE" image inspect "$img" >/dev/null 2>&1; then
      : # present locally — keep going
    else
      exit 1
    fi
  fi
  echo CONTINUED
}
out_present="$( run_pull_body eng_present || true )"
[ "$out_present" = "CONTINUED" ] \
  || { echo "FAIL: pull-fail + image-present must continue (got: '$out_present')"; exit 1; }
rc_absent=0
( run_pull_body eng_absent ) >/dev/null 2>&1 || rc_absent=$?
[ "$rc_absent" = "1" ] \
  || { echo "FAIL: pull-fail + image-absent must abort with rc=1 (got rc=$rc_absent)"; exit 1; }

echo "PASS: build-bundle assembles a verifiable tree (skip-images)"
