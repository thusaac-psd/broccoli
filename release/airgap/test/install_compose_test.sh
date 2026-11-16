#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
inst="$here/../install.sh"

# sources the shared runtime lib (DRY: no duplicate detection)
grep -q 'lib/runtime.sh' "$inst" || { echo "FAIL: install.sh does not source lib/runtime.sh"; exit 1; }
# resolves COMPOSE, reusing an export if present
grep -qE 'COMPOSE="\$\{COMPOSE:-\$\(runtime_compose' "$inst" \
  || { echo "FAIL: install.sh does not resolve COMPOSE via runtime_compose"; exit 1; }
# uses $COMPOSE (word-split, unquoted) for the compose up
grep -qE '\$COMPOSE \\?$|&& \$COMPOSE ' "$inst" \
  || { echo "FAIL: install.sh does not invoke \$COMPOSE"; exit 1; }
# no hardcoded 'docker compose' literal remains anywhere (no tech debt)
grep -q 'docker compose' "$inst" && { echo "FAIL: hardcoded 'docker compose' literal remains"; exit 1; } || true
# worker branch composes up the worker template via $COMPOSE, --pull never
grep -q 'docker-compose.worker.yaml.template' "$inst" \
  || { echo "FAIL: install.sh worker branch does not use the worker compose template"; exit 1; }
grep -q 'up -d --pull never' "$inst" \
  || { echo "FAIL: install.sh does not run compose up --pull never"; exit 1; }
# server bring-up restarts the gateway after `up -d` so a re-issued or rotated
# leaf — written to the SAME bind-mount path, which `up -d` won't recreate the
# gateway for — is loaded immediately (Caddy doesn't watch the cert files).
grep -qE '\$COMPOSE .*restart gateway' "$inst" \
  || { echo "FAIL: server role does not restart the gateway to reload a rotated leaf"; exit 1; }
# COMPOSE resolved exactly once (hoisted), not duplicated per branch
[ "$(grep -c 'runtime_compose' "$inst")" = "1" ] \
  || { echo "FAIL: COMPOSE should be resolved once (hoisted), found duplicates"; exit 1; }
# rootless Podman under SELinux Enforcing relabels bind-mount SOURCES or the
# container cannot read them. The server role bind-mounts ./plugins:/plugins;
# if its source dir is not relabeled the
# plugin registry loads EMPTY and nothing judges (the same catastrophic-silent
# class as an unreadable plugin dir). Assert install.sh relabels the plugins dir. runtime_relabel is a proven no-op off podman+SELinux
# (runtime_detect_test.sh), so this is safe for docker/standalone installs.
grep -qE 'runtime_relabel .*plugins' "$inst" \
  || { echo "FAIL: install.sh does not SELinux-relabel the ./plugins bind-mount source (podman+SELinux would read an empty plugin dir -> nothing judges)"; exit 1; }
# the server role relabels on both of its paths (with and without a gateway
# leaf) — expect a call site on each
[ "$(grep -c 'runtime_relabel' "$inst")" -ge 2 ] \
  || { echo "FAIL: install.sh must relabel bind mounts on both server-role paths"; exit 1; }
echo "PASS: install.sh migrated to \$COMPOSE"
