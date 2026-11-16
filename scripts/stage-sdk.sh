#!/usr/bin/env bash
# Stage the plugin SDKs into DEST/sdk, so a plugin author on a closed network
# scaffolds and builds against the bundle with no internet:
#
#   broccoli-dev plugin new my-plugin --sdk <bundle>/sdk
#   cd my-plugin && cargo build --offline --config <bundle>/sdk/.cargo/config.toml ...
#
# The layout is what `broccoli-dev plugin new --sdk` reads (a checkout's
# `packages/`, see packages/dev-cli/src/template/sdk.rs):
#
#   server-sdk/        broccoli-server-sdk, depending on ../broccoli-types
#   broccoli-types/    with its manifest normalized by `cargo package`, so it
#                      builds outside this workspace
#   web-sdk/           @broccoli/web-sdk as `pnpm pack` publishes it, built
#   vendor/            the crates.io closure of a scaffolded plugin crate
#   .cargo/config.toml the source replacement that points cargo at vendor/
#
# Runs on the connected build box (cargo vendor fetches crates.io once). It
# then checks, with an empty CARGO_HOME and --offline, that a crate rendered
# from the scaffold's own Cargo.toml template resolves from vendor/ alone.
#
# Usage: scripts/stage-sdk.sh DEST
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${1:?usage: stage-sdk.sh DEST}"
SDK="$DEST/sdk"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

rm -rf "$SDK"
mkdir -p "$SDK/server-sdk" "$SDK/broccoli-types" "$SDK/web-sdk"
SDK="$(cd "$SDK" && pwd)"

echo ">> staging the Rust SDK"
cp -R "$ROOT/packages/server-sdk/Cargo.toml" "$ROOT/packages/server-sdk/README.md" \
  "$ROOT/packages/server-sdk/src" "$SDK/server-sdk/"
# broccoli-types inherits workspace keys; `cargo package` writes the
# standalone manifest a published crate has.
cargo package --quiet --no-verify --allow-dirty -p broccoli-types --target-dir "$TMP/cargo"
mkdir -p "$TMP/types"
tar -xzf "$TMP"/cargo/package/broccoli-types-*.crate -C "$TMP/types"
cp -R "$TMP"/types/broccoli-types-*/. "$SDK/broccoli-types/"

echo ">> staging the web SDK"
pnpm --dir "$ROOT" --filter @broccoli/web-sdk build >/dev/null
# `pnpm pack` resolves the catalog: and workspace: versions to real ones.
mkdir -p "$TMP/web"
(cd "$ROOT/packages/web-sdk" && pnpm pack --pack-destination "$TMP/web" >/dev/null)
tar -xzf "$TMP"/web/broccoli-web-sdk-*.tgz -C "$TMP/web"
cp -R "$TMP"/web/package/. "$SDK/web-sdk/"

# probe NAME DIR: a plugin crate rendered from the scaffold's template,
# depending on the staged server SDK.
probe() {
  mkdir -p "$2/src"
  sed -e "s|{{plugin_name}}|$1|g" \
      -e "s|{{server_sdk_dep}}|path = \"$SDK/server-sdk\"|" \
      "$ROOT/packages/dev-cli/templates/backend/Cargo.toml.tmpl" > "$2/Cargo.toml"
  : > "$2/src/lib.rs"
}

echo ">> vendoring the crates.io closure"
probe sdk-probe "$TMP/probe"
cargo vendor --quiet --versioned-dirs --manifest-path "$TMP/probe/Cargo.toml" "$SDK/vendor" >/dev/null
mkdir -p "$SDK/.cargo"
cat > "$SDK/.cargo/config.toml" <<'EOF'
# Point cargo at the crates vendored in sdk/vendor instead of crates.io:
#   cargo build --offline --config <bundle>/sdk/.cargo/config.toml
# A relative `directory` here is relative to sdk/. To copy these lines into a
# plugin's own .cargo/config.toml, make `directory` absolute.
[source.crates-io]
replace-with = "broccoli-sdk-vendor"

[source.broccoli-sdk-vendor]
directory = "vendor"
EOF

echo ">> checking that a scaffolded plugin resolves offline"
probe offline-probe "$TMP/offline"
CARGO_HOME="$TMP/empty-cargo-home" cargo check --quiet --offline \
  --manifest-path "$TMP/offline/Cargo.toml" --config "$SDK/.cargo/config.toml" \
  --target-dir "$TMP/offline-target"
echo ">> sdk staged in $SDK ($(du -sh "$SDK" | cut -f1))"
