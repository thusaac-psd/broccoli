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
  echo "Usage: install.sh --role {server|gateway|worker|contestant} --bundle DIR [--lan-host H ...] [--server-secret DIR] [--burn-ca-key] [--dry-run]"
}

ROLE="" BUNDLE="" BURN=0 SECRET="" DRY_RUN=0
LAN_HOSTS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --role) ROLE="$2"; shift 2 ;;
    --bundle) BUNDLE="$2"; shift 2 ;;
    --lan-host) LAN_HOSTS+=("$2"); shift 2 ;;
    --server-secret) SECRET="$2"; shift 2 ;;
    --burn-ca-key) BURN=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown arg: $1" >&2; usage; exit 2 ;;
  esac
done
[ -n "$ROLE" ] && [ -n "$BUNDLE" ] || { echo "--role and --bundle are required" >&2; usage; exit 2; }

# Resolve the container engine + compose provider once for deploy roles
# (server/gateway/worker). Reuse setup.sh's export when present; else self-resolve
# (standalone use). Contestant does not deploy, so an empty result here is only
# fatal inside the deploy roles. ENGINE is also what gates the SELinux relabel below.
ENGINE="${BROCCOLI_ENGINE:-$(runtime_engine)}"
COMPOSE="${COMPOSE:-$(runtime_compose "$ENGINE")}"

os_helper() {
  case "$(uname -s)" in
    Linux)  echo "$here/trust-ca/linux.sh" ;;
    Darwin) echo "$here/trust-ca/macos.sh" ;;
    *) echo "" ;;
  esac
}

# gateway_tls_plan: point the gateway at the TLS leaf for LAN_HOSTS in the
# server-secret dir (default: the "<bundle>.server-secret" sibling
# build-bundle.sh writes). The gateway mounts caddy/Caddyfile.airgap
# UN-rendered and Caddy expands the {$VAR}s from its container env, so never
# shell-render that file. Pass the leaf cert+key as individual file paths, not
# the secret dir, so the contestant-facing gateway never bind-mounts root.key
# (the CA signing key that shares this dir).
gateway_tls_plan() {
  local h abs_bundle
  [ "${#LAN_HOSTS[@]}" -gt 0 ] || { echo "the TLS gateway needs --lan-host" >&2; exit 2; }
  for h in "${LAN_HOSTS[@]}"; do
    case "$h" in
      "" | *[!A-Za-z0-9.:-]*) echo "invalid LAN host '$h': only letters, digits, dot, colon, hyphen allowed" >&2; exit 2 ;;
    esac
  done
  abs_bundle="$(cd "$BUNDLE" && pwd)"
  SECRET="${SECRET:-${abs_bundle}.server-secret}"
  [ -d "$SECRET" ] || { echo "server-secret dir not found: $SECRET — deliver the '<bundle>.server-secret' dir to this host, or pass --server-secret DIR" >&2; exit 2; }
  abs_secret="$(cd "$SECRET" && pwd)"
  export LAN_HOST="${LAN_HOSTS[0]}" LAN_HOST_ALIASES="${LAN_HOSTS[*]:1}"
  export BROCCOLI_TLS_CERT="$abs_secret/server.crt" BROCCOLI_TLS_KEY="$abs_secret/server.key"
  echo "TLS gateway will serve https://$LAN_HOST${LAN_HOST_ALIASES:+ (also $LAN_HOST_ALIASES)} using leaf in $abs_secret"
}

# gateway_tls_issue: make the secret dir hold a current leaf for LAN_HOSTS.
# Keep a pre-issued or earlier one only if it chains to this CA, covers every
# host and is not about to expire; otherwise issue one now from the CA key that
# lives only here. A venue whose address moved therefore gets a new leaf.
gateway_tls_issue() {
  local h hosts=()
  for h in "${LAN_HOSTS[@]}"; do hosts+=(--host "$h"); done
  [ -f "$SECRET/root.crt" ] || cp "$BUNDLE/ca/root.crt" "$SECRET/root.crt"
  if ! bash "$here/ca/issue-leaf.sh" --check --ca-dir "$SECRET" "${hosts[@]}" --out "$SECRET"; then
    [ -f "$SECRET/root.key" ] || { echo "the leaf in $SECRET is missing, expiring, or does not cover ${LAN_HOSTS[*]}, and there is no $SECRET/root.key to issue one — re-run build-bundle.sh --lan-host ${LAN_HOSTS[0]}, or place root.key in the server-secret dir (TLS gateway host only)" >&2; exit 2; }
    bash "$here/ca/issue-leaf.sh" --ca-dir "$SECRET" "${hosts[@]}" --out "$SECRET"
  fi
  if [ "$BURN" = "1" ]; then rm -f "$SECRET/root.key"; echo "burned $SECRET/root.key"; fi
}

# edge_network: settle the edge network the gateway shares with a server on
# its host. The gateway sits at a fixed address (subnet base + 2) outside the
# range Docker hands out (base + 8, a /29), so a server can trust exactly that
# address. BROCCOLI_EDGE_SUBNET must be an IPv4 /28 or wider.
edge_network() {
  local base
  export BROCCOLI_EDGE_SUBNET="${BROCCOLI_EDGE_SUBNET:-172.31.250.0/28}"
  base="${BROCCOLI_EDGE_SUBNET%/*}"
  case "$base" in
    *[!0-9.]* | *.*.*.*.* | [!0-9]* ) echo "BROCCOLI_EDGE_SUBNET must be an IPv4 network, /28 or wider" >&2; exit 2 ;;
    *.*.*.*) ;;
    *) echo "BROCCOLI_EDGE_SUBNET must be an IPv4 network, /28 or wider" >&2; exit 2 ;;
  esac
  export BROCCOLI_GATEWAY_EDGE_IP="${BROCCOLI_GATEWAY_EDGE_IP:-${base%.*}.$((${base##*.} + 2))}"
  export BROCCOLI_EDGE_RANGE="${BROCCOLI_EDGE_RANGE:-${base%.*}.$((${base##*.} + 8))/29}"
}

# dry_run_plan VAR...: with --dry-run, print the exported settings and the
# compose command this role would run, then stop before loading images,
# issuing a leaf or starting anything. Both lines are shell-quoted, so a test
# can replay them.
dry_run_plan() {
  local v
  [ "$DRY_RUN" = 1 ] || return 0
  echo "== plan (dry-run) =="
  for v in "$@"; do printf 'export %s=%q\n' "$v" "${!v-}"; done
  printf 'compose dir: %q\n' "$BUNDLE/compose"
  printf 'compose:'; printf ' %q' "${compose_args[@]}" up -d --pull never; printf '\n'
  exit 0
}

case "$ROLE" in
  server)
    # Secrets are operator-supplied: the bundle ships .env.*.example only.
    infra_env="$BUNDLE/compose/.env.infra"
    server_env="$BUNDLE/compose/.env.server"
    # Two settings in .env.server (written by setup.sh, or by hand) shape a
    # multi-server deployment, so a re-run keeps the same shape:
    #   BROCCOLI_INFRA_HOST    this server joins that infra node's DB/MQ/S3 and
    #                          runs no infra of its own;
    #   BROCCOLI_GATEWAY_HOSTS the TLS gateway runs on those hosts (the
    #                          gateway role), not here.
    # The environment wins over the file.
    INFRA_HOST="${BROCCOLI_INFRA_HOST:-$(env_get "$server_env" BROCCOLI_INFRA_HOST)}"
    GATEWAY_HOSTS="${BROCCOLI_GATEWAY_HOSTS:-$(env_get "$server_env" BROCCOLI_GATEWAY_HOSTS)}"
    env_files=(.env.infra .env.server)
    [ -z "$INFRA_HOST" ] || env_files=(.env.server)
    for f in "${env_files[@]}"; do
      [ -f "$BUNDLE/compose/$f" ] || { echo "missing $BUNDLE/compose/$f — run setup.sh --role server, or copy $f.example to $f and fill in secrets (see release/docs/airgap-deployment.md)" >&2; exit 2; }
    done
    # The server stack keeps the project name it has always had (Compose's
    # default for this directory), so existing installs keep their volumes.
    # Pinned rather than defaulted so a COMPOSE_PROJECT_NAME in .env.infra or
    # .env.server cannot move it.
    compose_args=(-p compose)
    for f in "${env_files[@]}"; do compose_args+=(--env-file "$f"); done
    [ -n "$INFRA_HOST" ] || compose_args+=(-f docker-compose.infra.yaml.template)
    compose_args+=(-f docker-compose.server.yaml.template -f docker-compose.server-airgap.yaml.template)
    edge_network
    if [ -z "$GATEWAY_HOSTS" ]; then
      # The TLS gateway runs here. Validate the host BEFORE any heavy work
      # (bundle load, leaf issuance).
      [ "${#LAN_HOSTS[@]}" -gt 0 ] || { echo "server role requires --lan-host (or a gateway-only node: set BROCCOLI_GATEWAY_HOSTS, see setup.sh --gateway-host)" >&2; exit 2; }
      gateway_tls_plan
      compose_args+=(-f docker-compose.gateway-airgap.yaml.template)
      export BROCCOLI_UPSTREAMS="${BROCCOLI_UPSTREAMS:-server:3000}"
      # The 443 gateway is the only LAN ingress: bind the server's plaintext
      # :3000 to host loopback so no contestant on the LAN can bypass TLS.
      # Host-local operator ops still reach it. Universal across Compose
      # versions (an override `ports: !reset []` would hard-fail below Compose
      # v2.24, no offline remedy).
      export BROCCOLI_HTTP_BIND="${BROCCOLI_HTTP_BIND:-127.0.0.1:3000}"
      # Trust X-Forwarded-For only from the gateway's fixed address on the
      # edge network: not the whole subnet, which also holds the bridge.
      trust="$BROCCOLI_GATEWAY_EDGE_IP"
      echo "server plaintext :3000 bound to host loopback ($BROCCOLI_HTTP_BIND); 443 is the only LAN entrypoint"
    else
      # The gateway runs on its own host and reaches this server over the LAN,
      # so publish :3000 there and trust exactly the gateway's addresses.
      # Names resolve here: the server only ever sees the gateway's address.
      export BROCCOLI_HTTP_BIND="${BROCCOLI_HTTP_BIND:-0.0.0.0:3000}"
      trust="$GATEWAY_HOSTS"
      echo "server :3000 published on $BROCCOLI_HTTP_BIND for the gateway at $GATEWAY_HOSTS; firewall it to the gateway"
    fi
    BROCCOLI_GATEWAY_TRUSTED_PROXIES="$(gateway_trust_json "$trust")" \
      || { echo "BROCCOLI_GATEWAY_HOSTS must list IP addresses, CIDRs or resolvable names" >&2; exit 2; }
    export BROCCOLI_GATEWAY_TRUSTED_PROXIES
    echo "server trusts forwarded client addresses only from $BROCCOLI_GATEWAY_TRUSTED_PROXIES"
    [ -z "$INFRA_HOST" ] || echo "server uses the infra node at $INFRA_HOST; no db/redis/seaweedfs here"
    dry_run_plan LAN_HOST LAN_HOST_ALIASES BROCCOLI_TLS_CERT BROCCOLI_TLS_KEY BROCCOLI_UPSTREAMS \
      BROCCOLI_HTTP_BIND BROCCOLI_EDGE_SUBNET BROCCOLI_EDGE_RANGE BROCCOLI_GATEWAY_EDGE_IP BROCCOLI_GATEWAY_TRUSTED_PROXIES
    [ -n "$COMPOSE" ] || { echo "no working docker/podman compose provider found" >&2; exit 2; }
    bash "$here/load-bundle.sh" --bundle "$BUNDLE"
    [ -n "$GATEWAY_HOSTS" ] || gateway_tls_issue
    # Relabel every bind-mount SOURCE for rootless Podman under SELinux Enforcing
    # (no-op off podman+SELinux): the TLS leaf dir, the un-rendered Caddyfile, and
    # the ./plugins tree the server mounts read-only. Without the plugins relabel,
    # SELinux denies the container read access and the plugin registry loads empty
    # — infra HEALTHY but nothing judges. Runs before compose up so labels are set
    # before the containers mount them.
    enable_requested_plugins "$BUNDLE/compose"
    if [ -z "$GATEWAY_HOSTS" ]; then
      runtime_relabel "$ENGINE" "$abs_secret" "$here/caddy/Caddyfile.airgap" "$BUNDLE/compose/plugins" 2>/dev/null || true
    else
      runtime_relabel "$ENGINE" "$BUNDLE/compose/plugins" 2>/dev/null || true
    fi
    (
      cd "$BUNDLE/compose"
      $COMPOSE "${compose_args[@]}" up -d --pull never
      # Reload the gateway's leaf. `up -d` recreates a container only when its
      # spec changes; a re-issued or rotated leaf is written to the SAME
      # bind-mount path, so Compose sees no change and leaves the running
      # gateway (and its old in-memory leaf) as-is — Caddy does not watch the
      # cert files. Restart just the gateway so every server bring-up, including
      # a leaf/CA rotation, serves the current cert immediately. Cheap and
      # offline; no image swap, no data-volume touch.
      [ -n "$GATEWAY_HOSTS" ] || $COMPOSE "${compose_args[@]}" restart gateway
    )
    ;;
  gateway)
    # A gateway-only node: the internal-CA TLS gateway alone, balancing across
    # the servers in .env.gateway (setup.sh --role gateway writes it). It holds
    # the server-secret dir; servers behind it need none.
    gateway_env="$BUNDLE/compose/.env.gateway"
    [ -f "$gateway_env" ] || { echo "missing $gateway_env — run setup.sh --role gateway (or write LAN_HOST, LAN_HOST_ALIASES and BROCCOLI_UPSTREAMS there)" >&2; exit 2; }
    if [ "${#LAN_HOSTS[@]}" -eq 0 ]; then
      read -r -a LAN_HOSTS <<< "$(env_get "$gateway_env" LAN_HOST) $(env_get "$gateway_env" LAN_HOST_ALIASES)"
    fi
    BROCCOLI_UPSTREAMS="$(upstream_list "${BROCCOLI_UPSTREAMS:-$(env_get "$gateway_env" BROCCOLI_UPSTREAMS)}")" \
      || { echo "BROCCOLI_UPSTREAMS in $gateway_env must list the servers as HOST or HOST:PORT" >&2; exit 2; }
    export BROCCOLI_UPSTREAMS
    gateway_tls_plan
    edge_network
    echo "gateway balances across $BROCCOLI_UPSTREAMS, health-checked on /livez"
    trust="$(gateway_source_addrs "$BROCCOLI_UPSTREAMS")"
    cat <<EOF
Servers behind this gateway must trust it for X-Forwarded-For, or every
contestant shares one login rate limit. Install each server with:
  ./setup.sh --role server --gateway-host '${trust:-<this gateway LAN address>}' ...
EOF
    # Its own Compose project, so it never collides with a server stack.
    compose_args=(-p broccoli-gateway --env-file .env.gateway -f docker-compose.gateway-airgap.yaml.template)
    dry_run_plan LAN_HOST LAN_HOST_ALIASES BROCCOLI_TLS_CERT BROCCOLI_TLS_KEY BROCCOLI_UPSTREAMS \
      BROCCOLI_EDGE_SUBNET BROCCOLI_EDGE_RANGE BROCCOLI_GATEWAY_EDGE_IP
    [ -n "$COMPOSE" ] || { echo "no working docker/podman compose provider found" >&2; exit 2; }
    bash "$here/load-bundle.sh" --bundle "$BUNDLE"
    gateway_tls_issue
    runtime_relabel "$ENGINE" "$abs_secret" "$here/caddy/Caddyfile.airgap" 2>/dev/null || true
    (
      cd "$BUNDLE/compose"
      $COMPOSE "${compose_args[@]}" up -d --pull never
      # Load a re-issued leaf; see the server role.
      $COMPOSE "${compose_args[@]}" restart gateway
    )
    ;;
  worker)
    bash "$here/load-bundle.sh" --bundle "$BUNDLE"
    # build-bundle.sh stages the preflight into the bundle at native/.
    # --host-only: isolate and the compilers live in the worker image, so only
    # the host's kernel side (cgroup v2, systemd) is checked here.
    preflight="$here/native/live-boot-preflight.sh"
    if [ -x "$preflight" ]; then
      bash "$preflight" --host-only || echo "WARN: worker host preflight reported issues"
    else
      echo "WARN: sandbox preflight not found at $preflight — skipping go/no-go check" >&2
    fi
    # No CA trust step: a worker talks plain Postgres, Redis and S3 to the
    # server host and never makes a TLS connection to the gateway.
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
    worker_project="$(env_get "$worker_env" COMPOSE_PROJECT_NAME)"
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
