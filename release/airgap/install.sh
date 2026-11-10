#!/usr/bin/env bash
# Role dispatcher for an air-gapped broccoli LAN install. TARGET-SIDE:
# no network. All images come from the bundle; compose runs --pull never.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=/dev/null
. "$here/lib/runtime.sh"
# shellcheck source=lib/release-lib.sh
. "$here/lib/release-lib.sh"

usage() {
  echo "Usage: install.sh --role {server|worker|contestant} --bundle DIR [--lan-host H] [--server-secret DIR] [--burn-ca-key]"
}

ROLE="" BUNDLE="" LAN_HOST="" BURN=0 SECRET=""
while [ $# -gt 0 ]; do
  case "$1" in
    --role) ROLE="$2"; shift 2 ;;
    --bundle) BUNDLE="$2"; shift 2 ;;
    --lan-host) LAN_HOST="$2"; shift 2 ;;
    --server-secret) SECRET="$2"; shift 2 ;;
    --burn-ca-key) BURN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown arg: $1" >&2; usage; exit 2 ;;
  esac
done
[ -n "$ROLE" ] && [ -n "$BUNDLE" ] || { echo "--role and --bundle are required" >&2; usage; exit 2; }

# Resolve the container engine + compose provider once for deploy roles
# (server/worker). Reuse setup.sh's export when present; else self-resolve
# (standalone use). Contestant does not deploy, so an empty result here is only
# fatal inside server/worker. ENGINE is also what gates the SELinux relabel below.
ENGINE="${BROCCOLI_ENGINE:-$(runtime_engine)}"
COMPOSE="${COMPOSE:-$(runtime_compose "$ENGINE")}"

os_helper() {
  case "$(uname -s)" in
    Linux)  echo "$here/trust-ca/linux.sh" ;;
    Darwin) echo "$here/trust-ca/macos.sh" ;;
    *) echo "" ;;
  esac
}

case "$ROLE" in
  server)
    # Validate the host BEFORE any heavy work (bundle load, leaf issuance).
    [ -n "$LAN_HOST" ] || { echo "server role requires --lan-host" >&2; exit 2; }
    # Secrets are operator-supplied: the bundle ships .env.*.example only.
    infra_env="$BUNDLE/compose/.env.infra"
    server_env="$BUNDLE/compose/.env.server"
    for f in "$infra_env" "$server_env"; do
      [ -f "$f" ] || { echo "missing ${f} — copy ${f}.example to ${f} and fill in secrets (see release/docs/airgap-deployment.md)" >&2; exit 2; }
    done
    # Server-only secrets (CA + leaf private keys) live OUTSIDE the manifested
    # bundle tree. Default to the sidecar build-bundle.sh writes as a SIBLING of
    # the bundle dir ("<bundle>.server-secret"); override with --server-secret.
    abs_bundle="$(cd "$BUNDLE" && pwd)"
    SECRET="${SECRET:-${abs_bundle}.server-secret}"
    [ -d "$SECRET" ] || { echo "server-secret dir not found: $SECRET — deliver the '<bundle>.server-secret' dir to this host, or pass --server-secret DIR" >&2; exit 2; }
    bash "$here/load-bundle.sh" --bundle "$BUNDLE"
    # Ensure a TLS leaf exists in the secret dir: pre-issued at assembly, or
    # issue it now from the CA key that lives only here.
    if [ ! -f "$SECRET/server.crt" ] || [ ! -f "$SECRET/server.key" ]; then
      [ -f "$SECRET/root.key" ] || { echo "no leaf and no $SECRET/root.key to issue one — re-run build-bundle.sh --lan-host, or place root.key in the server-secret dir (server host only)" >&2; exit 2; }
      [ -f "$SECRET/root.crt" ] || cp "$BUNDLE/ca/root.crt" "$SECRET/root.crt"
      bash "$here/ca/issue-leaf.sh" --ca-dir "$SECRET" --host "$LAN_HOST" --out "$SECRET"
    fi
    if [ "$BURN" = "1" ]; then rm -f "$SECRET/root.key"; echo "burned $SECRET/root.key"; fi
    # The gateway mounts caddy/Caddyfile.airgap UN-rendered; Caddy expands the
    # {$VAR}s from its container env. Never shell-render this file first — that
    # would corrupt Caddy's {$VAR} placeholder syntax.
    export LAN_HOST
    export BROCCOLI_UPSTREAMS="${BROCCOLI_UPSTREAMS:-server:3000}"
    # Pass the leaf cert+key as individual file paths — NOT the secret dir — so
    # the contestant-facing gateway never bind-mounts root.key (the CA signing
    # key that shares this dir). See docker-compose.gateway-airgap.yaml.template.
    abs_secret="$(cd "$SECRET" && pwd)"
    export BROCCOLI_TLS_CERT="$abs_secret/server.crt"
    export BROCCOLI_TLS_KEY="$abs_secret/server.key"
    # The 443 gateway is the only LAN ingress: bind the server's plaintext :3000
    # to host loopback so no contestant on the LAN can bypass TLS. Host-local
    # operator ops still reach it. Universal across Compose versions (an override
    # `ports: !reset []` would hard-fail below Compose v2.24, no offline remedy).
    export BROCCOLI_HTTP_BIND="${BROCCOLI_HTTP_BIND:-127.0.0.1:3000}"
    echo "TLS gateway will serve https://$LAN_HOST using leaf in $abs_secret"
    echo "server plaintext :3000 bound to host loopback ($BROCCOLI_HTTP_BIND); 443 is the only LAN entrypoint"
    [ -n "$COMPOSE" ] || { echo "no working docker/podman compose provider found" >&2; exit 2; }
    # Relabel every bind-mount SOURCE for rootless Podman under SELinux Enforcing
    # (no-op off podman+SELinux): the TLS leaf dir, the un-rendered Caddyfile, and
    # the ./plugins tree the server mounts read-only. Without the plugins relabel,
    # SELinux denies the container read access and the plugin registry loads empty
    # — infra HEALTHY but nothing judges. Runs before compose up so labels are set
    # before the containers mount them.
    enable_requested_plugins "$BUNDLE/compose"
    runtime_relabel "$ENGINE" "$abs_secret" "$here/caddy/Caddyfile.airgap" "$BUNDLE/compose/plugins" 2>/dev/null || true
    (
      cd "$BUNDLE/compose"
      # The server stack keeps the project name it has always had (Compose's
      # default for this directory), so existing installs keep their volumes.
      # Pinned rather than defaulted so a COMPOSE_PROJECT_NAME in .env.infra or
      # .env.server cannot move it.
      compose_args=(
        -p compose
        --env-file .env.infra --env-file .env.server
        -f docker-compose.infra.yaml.template
        -f docker-compose.server.yaml.template
        -f docker-compose.gateway-airgap.yaml.template
      )
      $COMPOSE "${compose_args[@]}" up -d --pull never
      # Reload the gateway's leaf. `up -d` recreates a container only when its
      # spec changes; a re-issued or rotated leaf is written to the SAME
      # bind-mount path, so Compose sees no change and leaves the running
      # gateway (and its old in-memory leaf) as-is — Caddy does not watch the
      # cert files. Restart just the gateway so every server bring-up, including
      # a leaf/CA rotation, serves the current cert immediately. Cheap and
      # offline; no image swap, no data-volume touch.
      $COMPOSE "${compose_args[@]}" restart gateway
    )
    ;;
  worker)
    bash "$here/load-bundle.sh" --bundle "$BUNDLE"
    # build-bundle.sh stages the preflight into the bundle at native/.
    preflight="$here/native/live-boot-preflight.sh"
    if [ -x "$preflight" ]; then
      bash "$preflight" || echo "WARN: worker sandbox preflight reported issues"
    else
      echo "WARN: sandbox preflight not found at $preflight — skipping go/no-go check" >&2
    fi
    # Surface a genuine trust failure; only unsupported-OS is a soft warning.
    helper="$(os_helper)"
    if [ -n "$helper" ]; then
      bash "$helper" "$BUNDLE/ca/root.crt" || { echo "ERROR: CA trust failed" >&2; exit 1; }
    else
      echo "WARN: unsupported OS for CA trust helper; trust $BUNDLE/ca/root.crt manually" >&2
    fi
    # Bring the worker up against the server's LAN infra. .env.worker is
    # rendered by setup.sh (secrets from the cluster-secret sidecar).
    worker_env="$BUNDLE/compose/.env.worker"
    [ -f "$worker_env" ] || { echo "missing $worker_env — run setup.sh --role worker (or copy compose/.env.worker.example and fill in secrets)" >&2; exit 2; }
    [ -n "$COMPOSE" ] || { echo "no working docker/podman compose provider found" >&2; exit 2; }
    # ./plugins:/plugins:ro is bind-mounted from the bundle; build-bundle.sh
    # stages the real plugin set there (manifest-verified above), so the source
    # dir already exists and carries the evaluators/checkers the worker runs.
    # Relabel that source for rootless Podman under SELinux Enforcing (no-op
    # otherwise) — else SELinux denies the read and the worker loads no
    # evaluators/checkers. Runs before compose up so the label is set first.
    enable_requested_plugins "$BUNDLE/compose"
    runtime_relabel "$ENGINE" "$BUNDLE/compose/plugins" 2>/dev/null || true
    # A worker is its own Compose project (COMPOSE_PROJECT_NAME in a newly
    # generated .env.worker), so a worker on the server host gets its own
    # network and is never an orphan of the server stack. A .env.worker from
    # before that keeps the old shared name, and with it its volumes.
    worker_project="$(sed -n "s/^COMPOSE_PROJECT_NAME=['\"]\{0,1\}\([^'\"]*\).*/\1/p" "$worker_env" | tail -n 1)"
    ( cd "$BUNDLE/compose" && $COMPOSE -p "${worker_project:-compose}" --env-file .env.worker \
        -f docker-compose.worker.yaml.template up -d --pull never )
    echo "worker started against server infra (compose up --pull never)"
    ;;
  contestant)
    # Re-verify bundle integrity before trusting a CA or installing a binary
    # onto PATH — this is the client-side trust boundary after USB transfer.
    # --pristine: a contestant bundle NEVER carries on-host env config (it's
    # excluded from the manifest); one present signals tampering, so refuse.
    bash "$here/load-bundle.sh" --bundle "$BUNDLE" --verify-only --pristine
    helper="$(os_helper)"
    [ -n "$helper" ] || { echo "unsupported OS for contestant trust helper" >&2; exit 1; }
    bash "$helper" "$BUNDLE/ca/root.crt"
    if [ -f "$BUNDLE/cli/broccoli" ]; then
      sudo install -m 0755 "$BUNDLE/cli/broccoli" /usr/local/bin/broccoli
      echo "installed contestant CLI -> /usr/local/bin/broccoli"
    fi
    ;;
  *) echo "unknown role: $ROLE" >&2; usage; exit 2 ;;
esac
