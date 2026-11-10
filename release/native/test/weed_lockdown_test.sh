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
echo "PASS: native SeaweedFS listens on loopback; only S3 reaches the LAN"
