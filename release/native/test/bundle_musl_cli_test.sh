#!/usr/bin/env bash
# Static assertions on build-native-bundle.sh: the contestant CLI must be
# collected as the musl-static artifact (portable on any distro / glibc 2.31),
# NEVER the host-glibc target/release/broccoli. Pure text checks -- no build.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
script="$here/../build-native-bundle.sh"

fail=0
check()   { grep -Eq -- "$1" "$script" || { echo "MISSING: $1"; fail=1; }; }
refute()  { grep -Eq -- "$1" "$script" && { echo "PRESENT (must be gone): $1"; fail=1; }; return 0; }

# The musl triple is single-sourced as a shell var (DRY), not inline-repeated.
check '^[[:space:]]*MUSL_TARGET="?x86_64-unknown-linux-musl"?'
# The CLI is collected from the musl release-cli output, via the var.
check 'target/\$\{?MUSL_TARGET\}?/release-cli/broccoli'
# The bundle never copies a host-glibc CLI build.
refute 'target/release/broccoli"?[[:space:]]*"\$WORK/cli/broccoli'

[ "$fail" -eq 0 ] && echo "PASS: bundle collects musl-static CLI" || { echo "FAIL"; exit 1; }
