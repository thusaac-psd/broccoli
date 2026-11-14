# shellcheck shell=bash
# stage_debs NAME SETUP PACKAGES [UBUNTU_VERSION...]: download PACKAGES and
# their dependency closure, without installing them, inside a fresh
# ubuntu:<ver> amd64 container for each version (default 20.04 22.04 24.04),
# into release/native/NAME/<ver>/*.deb. SETUP is shell run in the container
# first (adding a vendor APT repo, say). install-native.sh installs the set
# matching the box's VERSION_ID with dpkg, so a box in a closed lab needs no
# network. The closure is resolved against a minimal base image, so it is a
# superset of what a fuller install needs; dpkg skips what is installed.
#
# Sourced by stage-toolchain.sh, stage-redis.sh and stage-postgres.sh. Needs
# Docker and internet; the target Ubuntu version need not match the host.
stage_debs() {
  local name="$1" setup="$2" packages="$3"
  shift 3
  local vers=("$@") ver dest n
  [ ${#vers[@]} -gt 0 ] || vers=(20.04 22.04 24.04)
  command -v docker >/dev/null 2>&1 || {
    echo "error: docker not found; run this on a box with Docker and internet." >&2
    return 1
  }
  for ver in "${vers[@]}"; do
    dest="$STAGE_DIR/$name/$ver"
    mkdir -p "$dest"
    echo ">> staging $name for Ubuntu $ver -> $dest"
    rm -f "$dest"/*.deb
    # --platform pins amd64: the whole bundle is x86_64, so the .debs must be
    # amd64 even when staging from an arm64 host (qemu-emulated).
    docker run --rm --platform linux/amd64 -v "$dest:/out" -e SETUP="$setup" -e PACKAGES="$packages" \
      "ubuntu:$ver" bash -c '
        set -e
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        # Resolve against the base image as it was before SETUP installed its
        # own tools, so what they pulled in is part of the closure.
        cp /var/lib/dpkg/status /tmp/base-status
        eval "$SETUP"
        apt-get clean
        # shellcheck disable=SC2086
        apt-get install -y --download-only --no-install-recommends \
          -o Dir::State::status=/tmp/base-status $PACKAGES
        cp /var/cache/apt/archives/*.deb /out/ 2>/dev/null || true
        chmod 644 /out/*.deb 2>/dev/null || true
      ' || { echo "  WARN: staging $name for $ver failed (image pull or apt); skipping." >&2; continue; }
    n=$(find "$dest" -name '*.deb' | wc -l | tr -d ' ')
    if [ "$n" = 0 ]; then
      echo "  WARN: no .debs captured for $ver" >&2
    else
      echo "  staged $ver: $n debs ($(du -sh "$dest" | cut -f1))"
    fi
  done
  echo ">> done. $name sets under $STAGE_DIR/$name/ ; rebuild the bundle to include them."
}
