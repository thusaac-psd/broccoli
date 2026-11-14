#!/usr/bin/env bash
# Stage the C/C++/Python toolchain (build-essential + python3 closure) as offline
# .deb sets for live-boot / airgapped Broccoli WORKER boxes. install-native.sh
# (worker role) auto-installs the set matching the box's Ubuntu VERSION_ID via
# `dpkg -i toolchain/<ver>/*.deb` before falling back to apt.
#
# Run on ANY machine with Docker + internet (it does NOT need to match the target
# Ubuntu version -- each set is built inside that version's container):
#   ./stage-toolchain.sh                 # stages 20.04 22.04 24.04 (default)
#   ./stage-toolchain.sh 22.04           # just one version
# Output: release/native/toolchain/<ver>/*.deb -- folded into the contest bundle by
# build-native-bundle.sh. Re-run any time to refresh.
#
# NO DOCKER? On an online box that IS the target version, capture it natively:
#   sudo apt-get update
#   sudo apt-get install -y --download-only build-essential python3
#   v=$(. /etc/os-release; echo "$VERSION_ID"); mkdir -p "toolchain/$v"
#   cp /var/cache/apt/archives/*.deb "toolchain/$v/"
#
# Note: the closure is resolved against a MINIMAL base image, so it is a superset
# of what a full Desktop live ISO needs (those extra libs dpkg-skip as already
# installed). Within one LTS this is robust; across a major point-release skew the
# self-install path (apt / LAN mirror) remains the always-correct fallback.
set -euo pipefail
STAGE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=stage-debs.sh
. "$STAGE_DIR/stage-debs.sh"
stage_debs toolchain "" "build-essential python3" "$@"
