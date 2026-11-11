#!/usr/bin/env bash
# Per-role go/no-go for an air-gap install. Emits PASS/WARN/FAIL lines; the
# aggregate rc is nonzero iff any FAIL was emitted. TARGET-SIDE: no network.
set -euo pipefail
_pf_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
. "$_pf_dir/runtime.sh"
# shellcheck source=/dev/null
. "$_pf_dir/manifest.sh"

_pf_fail=0
_pf_pass() { echo "PASS: $*"; }
_pf_warn() { echo "WARN: $*"; }
_pf_bad()  { echo "FAIL: $*"; _pf_fail=1; }

_pf_disk() {
  local path="$1" kb
  kb="$(df -Pk "$path" 2>/dev/null | awk 'NR==2{print $4}')" || kb=""
  if [ -n "$kb" ] && [ "$kb" -ge 2097152 ]; then _pf_pass "disk: $((kb/1024)) MiB free"
  else _pf_warn "low disk (<2 GiB free) at $path"; fi
}

_pf_server() {
  local secret="$1"
  if [ -n "$secret" ] && { { [ -f "$secret/server.crt" ] && [ -f "$secret/server.key" ]; } || [ -f "$secret/root.key" ]; }; then
    _pf_pass "server TLS material present"
  else
    _pf_bad "server-secret dir needs server.crt+server.key or root.key (got: ${secret:-none})"
  fi
}

_pf_cluster() {
  local cluster="$1" role="$2"
  if [ -n "$cluster" ] && [ -f "$cluster/cluster-secrets.env" ]; then
    _pf_pass "cluster-secret present"; return
  fi
  if [ "$role" = worker ]; then
    _pf_bad "worker needs the cluster-secret sidecar (cluster-secrets.env) — deliver '<bundle>.cluster-secret' or pass --cluster-secret (got: ${cluster:-none})"
  else
    _pf_warn "no cluster-secret sidecar; server will generate fresh secrets and workers cannot join it (got: ${cluster:-none})"
  fi
}

_pf_worker() {
  local pf="$_pf_dir/../native/live-boot-preflight.sh"
  if [ -x "$pf" ]; then
    if bash "$pf" --host-only >/dev/null 2>&1; then _pf_pass "worker host preflight (cgroup v2, systemd)"
    else _pf_warn "worker host preflight reported issues (cgroup v2 or systemd; run it as root to see which)"; fi
  else
    _pf_warn "sandbox preflight not staged; run go/no-go manually"
  fi
  if grep -qi microsoft /proc/version 2>/dev/null; then
    _pf_warn "WSL kernel: isolate needs cgroup/namespace features WSL2 may lack"
  fi
}

_pf_contestant() {
  case "$(uname -s)" in
    Linux|Darwin) _pf_pass "OS trust helper available" ;;
    *) _pf_warn "no trust helper for $(uname -s); trust root.crt manually" ;;
  esac
}

_pf_join() {
  if [ -f "$1" ]; then _pf_pass "server-join.env from the infra node present"
  else _pf_bad "a server joining an infra node needs its server-join.env — copy compose/server-join.env from the infra node, or pass --server-join (got: $1)"; fi
}

# preflight_run ROLE BUNDLE [SECRET] [CLUSTER] [JOIN] [REMOTE_GATEWAY]
# For the server role, JOIN names the infra node's server-join.env when this
# server joins an infra node instead of running infra itself, and
# REMOTE_GATEWAY=1 means the TLS gateway runs on another host, so this one
# needs no TLS material.
preflight_run() {
  local role="$1" bundle="$2" secret="${3:-}" cluster="${4:-}" join="${5:-}" remote_gw="${6:-}"
  _pf_fail=0
  local engine compose=""
  engine="$(runtime_engine)"
  if [ -n "$engine" ]; then _pf_pass "container engine: $engine"
  else _pf_bad "no working docker or podman found (install Docker, or Podman on RHEL-family)"; fi
  [ -n "$engine" ] && compose="$(runtime_compose "$engine")"
  if [ -n "$compose" ]; then _pf_pass "compose: $compose"
  else _pf_bad "no compose provider ('docker compose' / 'podman compose' / 'podman-compose')"; fi
  if [ -d "$bundle" ] && [ "$(manifest_verify "$bundle" 2>/dev/null)" = OK ]; then _pf_pass "bundle integrity"
  else _pf_bad "bundle integrity check failed for ${bundle:-none}"; fi
  _pf_disk "$bundle"
  case "$role" in
    server)
      [ "$remote_gw" = 1 ] || _pf_server "$secret"
      if [ -n "$join" ]; then _pf_join "$join"; else _pf_cluster "$cluster" server; fi
      ;;
    gateway)    _pf_server "$secret" ;;
    worker)     _pf_worker; _pf_cluster "$cluster" worker ;;
    contestant) _pf_contestant ;;
    *)          _pf_bad "unknown role: $role" ;;
  esac
  return "$_pf_fail"
}
