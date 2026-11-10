#!/usr/bin/env bash
# Install the broccoli LAN root CA into the Linux system trust store, on
# Debian/Ubuntu (update-ca-certificates), SUSE (update-ca-certificates with
# /etc/pki/trust/anchors) and RHEL/Rocky/Fedora/Arch (update-ca-trust).
# Runs as root directly, or through sudo otherwise. Idempotent. TARGET-SIDE:
# no network.
set -euo pipefail
CRT="${1:?usage: linux.sh <root.crt>}"
name="broccoli-lan-root.crt"

as_root() {
  if [ "$(id -u)" = 0 ]; then "$@"; else sudo "$@"; fi
}

if command -v update-ca-certificates >/dev/null 2>&1 && [ -d /usr/local/share/ca-certificates ]; then
  dir=/usr/local/share/ca-certificates refresh=(update-ca-certificates)
elif command -v update-ca-certificates >/dev/null 2>&1 && [ -d /etc/pki/trust/anchors ]; then
  dir=/etc/pki/trust/anchors refresh=(update-ca-certificates)
elif command -v update-ca-trust >/dev/null 2>&1 && [ -d /etc/pki/ca-trust/source/anchors ]; then
  dir=/etc/pki/ca-trust/source/anchors refresh=(update-ca-trust extract)
elif command -v update-ca-trust >/dev/null 2>&1 && [ -d /etc/ca-certificates/trust-source/anchors ]; then
  dir=/etc/ca-certificates/trust-source/anchors refresh=(update-ca-trust extract)
else
  echo "error: no system CA store found (need update-ca-certificates or update-ca-trust); trust $CRT manually" >&2
  exit 1
fi

as_root install -m 0644 "$CRT" "$dir/$name"
as_root "${refresh[@]}"
echo "installed root CA -> $dir/$name"
echo "note: Firefox uses its own cert store; import $CRT there separately if needed."
