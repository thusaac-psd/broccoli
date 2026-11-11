#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$here/.."

# Target-side scripts that run on the air-gapped LAN. build-bundle.sh is
# STAGING-side and intentionally excluded.
targets=(
  "$root/load-bundle.sh"
  "$root/install.sh"
  "$root/ca/issue-leaf.sh"
  "$root/ca/sign.sh"
  "$root/lib/release-lib.sh"
  "$root/../lib/env.sh"
  "$root/../lib/host.sh"
  "$root/trust-ca/linux.sh"
  "$root/trust-ca/macos.sh"
  "$root/lib/manifest.sh"
  "$root/setup.sh"
  "$root/lib/runtime.sh"
  "$root/lib/answers.sh"
  "$root/lib/envgen.sh"
  "$root/lib/preflight.sh"
)
[ "${#targets[@]}" -gt 0 ] || { echo "FAIL: empty target list"; exit 1; }

rc=0
for f in "${targets[@]}"; do
  [ -f "$f" ] || { echo "FAIL: missing $f"; rc=1; continue; }
  if grep -Eq '\b(curl|wget|apt|apt-get|pip[0-9]*)\b' "$f"; then
    echo "FAIL: $f contains a network fetch"; rc=1
  fi
  # docker/podman pull is only allowed with --pull never
  if grep -Eq '(docker|podman)[[:space:]]+(image[[:space:]]+)?pull' "$f"; then
    echo "FAIL: $f uses 'docker/podman pull'"; rc=1
  fi
done

# The Windows trust helper is target-side but PowerShell; bash fetch idioms
# don't apply, so check it for PowerShell/Windows download idioms separately.
win="$root/trust-ca/windows.ps1"
if [ -f "$win" ]; then
  if grep -Eqi 'invoke-webrequest|invoke-restmethod|net\.webclient|downloadfile|downloadstring|start-bitstransfer|bitsadmin|\b(iwr|irm|curl|wget)\b|certutil[^|]*-urlcache' "$win"; then
    echo "FAIL: $win contains a network fetch"; rc=1
  fi
else
  echo "FAIL: missing $win"; rc=1
fi

[ "$rc" = "0" ] && echo "PASS: no target-side script fetches from the network"
exit $rc
