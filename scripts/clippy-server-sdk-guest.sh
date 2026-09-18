#!/usr/bin/env bash
# Runs Clippy (`-D warnings`) for `broccoli-server-sdk` with its `guest`
# feature enabled.
#
# `guest` (`extism-pdk` + `broccoli-types/guest`) is off by default
# (`default = []` in packages/server-sdk/Cargo.toml) and nothing IN the root
# workspace turns it on: `packages/server` depends on this crate with
# default features only, so `cargo clippy --workspace --all-targets` never
# compiles the WASM guest-side code paths this feature gates (e.g. the
# `#[cfg(target_arch = "wasm32")]` branch in `sdk/eval.rs`, or the
# `#[cfg(test)] mod tests` blocks that only exist under `guest`).
#
# Every plugin crate under `plugins/` DOES enable `guest` on this crate, as
# a path dependency - but `scripts/clippy-plugins.sh` only lints each
# plugin's own crate. Clippy's `-D warnings` only promotes warnings to a
# build failure for the crate(s) being *directly* linted (`-p`/
# `--manifest-path`), not their dependencies, so server-sdk's own warnings
# under `guest` are silently compiled, never failed on, by that script
# either - confirmed by reproducing this exact gap (see the D-2 batch report
# for the break/restore proof: `cargo clippy --manifest-path
# plugins/icpc/Cargo.toml --all-targets -- -D warnings` printed server-sdk's
# warnings but exited 0, while this script's invocation, run directly
# against the same broken state, exited 101).
#
# This is a separate, sibling script rather than folded into
# `clippy-plugins.sh` because that script's whole shape (the `find`-based
# discovery loop over `plugins/`) exists to solve "there are N plugin crates
# under plugins/, don't hardcode the list" - a problem this single, fixed,
# non-plugins-dir crate + single fixed feature flag does not have.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT_DIR}"

cargo clippy -p broccoli-server-sdk --features guest --all-targets --locked -- -D warnings
