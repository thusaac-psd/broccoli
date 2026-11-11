#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
d="$here/../trust-ca"

for f in linux.sh macos.sh; do
  [ -f "$d/$f" ] || { echo "FAIL: $f missing"; exit 1; }
  bash -n "$d/$f" || { echo "FAIL: $f does not parse"; exit 1; }
done
[ -f "$d/windows.ps1" ] || { echo "FAIL: windows.ps1 missing"; exit 1; }

grep -q 'update-ca-certificates'        "$d/linux.sh"   || { echo "FAIL: linux.sh lacks update-ca-certificates"; exit 1; }
grep -q 'update-ca-trust extract'       "$d/linux.sh"   || { echo "FAIL: linux.sh lacks the RHEL-family update-ca-trust"; exit 1; }
grep -q 'add-trusted-cert'              "$d/macos.sh"   || { echo "FAIL: macos.sh lacks add-trusted-cert"; exit 1; }
grep -qi 'certutil'                      "$d/windows.ps1"|| { echo "FAIL: windows.ps1 lacks certutil"; exit 1; }
grep -qi 'Root'                          "$d/windows.ps1"|| { echo "FAIL: windows.ps1 lacks Root store"; exit 1; }

# offline: helpers must not fetch anything
for f in linux.sh macos.sh; do
  grep -Eq '\b(curl|wget|apt|apt-get|pip[0-9]*)\b' "$d/$f" && { echo "FAIL: $f fetches network"; exit 1; }
done
echo "PASS: trust-ca helpers present, parse, target the OS root store, offline"
