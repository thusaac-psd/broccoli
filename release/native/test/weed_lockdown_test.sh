#!/usr/bin/env bash
# Native SeaweedFS exposes only its credentialed S3 API on the LAN. The
# master, volume and filer (which serves every testcase without credentials)
# stay on loopback, and a systemd socket forwards LAN :8333 to S3 on loopback.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
sd="$here/../systemd"
fail() { echo "FAIL: $*"; exit 1; }

weed="$(grep '^ExecStart=' "$sd/broccoli-weed.service.tmpl")"
case "$weed" in *0.0.0.0* | *@WEED_IP@*) fail "SeaweedFS binds a LAN address: $weed" ;; esac
for flag in -ip=127.0.0.1 -ip.bind=127.0.0.1 -s3.port=8334 -s3.port.iceberg=0; do
  case " $weed " in *" $flag "*) ;; *) fail "SeaweedFS unit lacks $flag" ;; esac
done
grep -qx 'ListenStream=@WEED_IP@:8333' "$sd/broccoli-weed-s3.socket.tmpl" || fail "S3 socket does not listen on the LAN :8333"
grep -q '^ExecStart=@SOCKET_PROXYD@ .*127.0.0.1:8334$' "$sd/broccoli-weed-s3.service.tmpl" \
  || fail "S3 forwarder does not target S3 on 127.0.0.1:8334"
inst="$here/../install-native.sh"
grep -q 'broccoli-weed.service broccoli-weed-s3.socket broccoli-weed-s3.service' "$inst" || fail "installer does not render all three units"
grep -q 'systemctl enable broccoli-weed broccoli-weed-s3.socket' "$inst" || fail "installer does not enable the S3 socket"

# The unit's flags are written for the SeaweedFS release the bundle pins: the
# Docker templates' tag. The builder refuses another release unless told to,
# and the installer drops -s3.port.iceberg for a weed that does not know it
# (weed refuses to start on an unknown flag).
pin="$(sed -n 's/^SEAWEEDFS_IMAGE=.*:\([^:]*\)$/\1/p' "$here/../../.env.infra.example")"
[ -n "$pin" ] || fail "no SeaweedFS tag to pin to"
grep -q 'weed_pin=.*SEAWEEDFS_IMAGE.*\.env\.infra\.example' "$here/../build-native-bundle.sh" \
  || fail "the native bundle does not pin weed to the Docker templates' SeaweedFS tag"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
# shellcheck source=/dev/null
. <(sed -n '/^weed_drop_unknown_flags(){/,/^}/p' "$inst")
mkdir -p "$T/bin"
for kind in new old; do
  if [ "$kind" = new ]; then usage='  -s3.port.iceberg int'; else usage='  -s3.port int'; fi
  printf '#!/bin/sh\necho "%s" >&2\nexit 2\n' "$usage" > "$T/bin/weed"; chmod +x "$T/bin/weed"
  printf '%s\n' "$weed" > "$T/$kind.service"
  BUNDLE="$T" weed_drop_unknown_flags "$T/$kind.service" >/dev/null
done
grep -q -- ' -s3.port.iceberg=0' "$T/new.service" || fail "the Iceberg port is no longer disabled on a weed that has one"
grep -q -- 'iceberg' "$T/old.service" && fail "a weed without -s3.port.iceberg would refuse the flag"
grep -qE -- ' -s3\.port=8334( |$)' "$T/old.service" || fail "dropping the Iceberg flag touched another flag"
echo "PASS: native SeaweedFS listens on loopback; only S3 reaches the LAN"
