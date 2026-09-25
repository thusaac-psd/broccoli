# List available recipes
default:
    @just --list

# Build all workspace crates
build:
    cargo build

# Build server and worker with opt-in profiling symbols and frame pointers.
# This intentionally does not affect the default build/release recipes.
build-profiling *args:
    RUSTFLAGS="-C force-frame-pointers=yes" cargo build --profile profiling -p server -p worker {{args}}

# Build a single crate with opt-in profiling symbols and frame pointers.
build-profiling-crate crate *args:
    RUSTFLAGS="-C force-frame-pointers=yes" cargo build --profile profiling -p {{crate}} {{args}}

# Run baseline Rust workspace tests.
# This excludes root plugins (they are intentionally not workspace members) and
# the stress-test harness. Use test-plugins/test-stress for those opt-in suites.
test:
    cargo test --workspace --exclude stress-test

# Run every Rust workspace test, including the stress-test harness
test-workspace-all:
    cargo test --workspace

# Test a single crate (e.g., just test-crate plugin-core)
test-crate crate:
    cargo test -p {{crate}}

# Test a root plugin crate by manifest path (e.g., just test-plugin plugins/ioi)
test-plugin path:
    cargo test --manifest-path {{path}}/Cargo.toml

# Test all root plugin crates explicitly; root plugins stay out of Cargo workspace
test-plugins:
    cargo test --manifest-path plugins/batch-evaluator/Cargo.toml
    cargo test --manifest-path plugins/codelink/Cargo.toml
    cargo test --manifest-path plugins/communication-evaluator/Cargo.toml
    cargo test --manifest-path plugins/cooldown/Cargo.toml
    cargo test --manifest-path plugins/icpc/Cargo.toml
    cargo test --manifest-path plugins/ioi/Cargo.toml
    cargo test --manifest-path plugins/print/Cargo.toml
    cargo test --manifest-path plugins/standard-checkers/Cargo.toml
    cargo test --manifest-path plugins/standard-languages/Cargo.toml
    cargo test --manifest-path plugins/submission-limit/Cargo.toml

# Run server tests that require opt-in bundled-stress-test feature
test-server-bundled-downloads:
    cargo test -p server --features bundled-stress-test routes::downloads

# Run stress-test harness tests explicitly
test-stress:
    cargo test -p stress-test

# Run clippy on all workspace crates
clippy:
    cargo clippy --workspace

# Run the backend server
server:
    cargo run -p server

# Run the worker
worker:
    cargo run -p worker

# Install all JS dependencies
install:
    pnpm install

# Generate API client from OpenAPI spec
gen-api url="http://127.0.0.1:3000/api-docs/openapi.json":
    node packages/web-sdk/scripts/generate-api.mjs {{url}}

# Build all JS packages (sdk before web)
build-js:
    pnpm build

# Build frontend SDK only
build-sdk:
    pnpm --filter @broccoli/web-sdk build

# Preview frontend
preview:
    pnpm --filter @broccoli/web preview

# Dev server for frontend
dev-web:
    pnpm --filter @broccoli/web dev

# All packages in parallel dev mode
dev:
    pnpm dev

# ESLint
lint-js:
    pnpm lint

# Prettier format
format:
    pnpm format

# Prettier format check
format-check:
    pnpm format:check

# Build all WASM plugins (debug)
build-plugins *args:
    cargo run -p broccoli-dev-cli -- plugin build plugins/batch-evaluator {{args}}
    cargo run -p broccoli-dev-cli -- plugin build plugins/codelink {{args}}
    cargo run -p broccoli-dev-cli -- plugin build plugins/communication-evaluator {{args}}
    cargo run -p broccoli-dev-cli -- plugin build plugins/cooldown {{args}}
    cargo run -p broccoli-dev-cli -- plugin build plugins/icpc {{args}}
    cargo run -p broccoli-dev-cli -- plugin build plugins/ioi {{args}}
    cargo run -p broccoli-dev-cli -- plugin build plugins/print {{args}}
    cargo run -p broccoli-dev-cli -- plugin build plugins/standard-checkers {{args}}
    cargo run -p broccoli-dev-cli -- plugin build plugins/standard-languages {{args}}
    cargo run -p broccoli-dev-cli -- plugin build plugins/submission-limit {{args}}

# Build all WASM plugins (release)
build-plugins-release:
    cargo run -p broccoli-dev-cli -- plugin build plugins/batch-evaluator --install --release
    cargo run -p broccoli-dev-cli -- plugin build plugins/codelink --install --release
    cargo run -p broccoli-dev-cli -- plugin build plugins/communication-evaluator --install --release
    cargo run -p broccoli-dev-cli -- plugin build plugins/cooldown --install --release
    cargo run -p broccoli-dev-cli -- plugin build plugins/icpc --install --release
    cargo run -p broccoli-dev-cli -- plugin build plugins/ioi --install --release
    cargo run -p broccoli-dev-cli -- plugin build plugins/print --install --release
    cargo run -p broccoli-dev-cli -- plugin build plugins/standard-checkers --install --release
    cargo run -p broccoli-dev-cli -- plugin build plugins/standard-languages --install --release
    cargo run -p broccoli-dev-cli -- plugin build plugins/submission-limit --install --release

# Build a single WASM plugin (e.g., just build-plugin plugins/standard-checkers)
build-plugin path *args:
    cargo run -p broccoli-dev-cli -- plugin build {{path}} {{args}}

# Build a single WASM plugin in release mode
build-plugin-release path:
    cargo run -p broccoli-dev-cli -- plugin build {{path}} --install --release

# Build the native print-station client (release binary)
build-print-client:
    cargo build --release --manifest-path plugins/print/client/Cargo.toml

# Run the native print-station client (e.g. just print-client setup)
print-client *args:
    cargo run --manifest-path plugins/print/client/Cargo.toml -- {{args}}

# Start PostgreSQL, Redis, and SeaweedFS
up:
    docker compose up -d

# Stop all services
down:
    docker compose down

# Dry-run publish the shared types crate to crates.io
publish-types-dry:
    cargo publish -p broccoli-types --dry-run

# Publish the shared types crate to crates.io (must precede publish-server-sdk)
publish-types:
    cargo publish -p broccoli-types

# Dry-run publish server SDK to crates.io
publish-server-sdk-dry:
    cargo publish -p broccoli-server-sdk --dry-run

# Publish server SDK to crates.io (run publish-types first: it is a dependency)
publish-server-sdk:
    cargo publish -p broccoli-server-sdk

# Dry-run publish CLI to crates.io
publish-cli-dry:
    cargo publish -p broccoli-dev-cli --dry-run

# Publish CLI to crates.io
publish-cli:
    cargo publish -p broccoli-dev-cli

# Run baseline checks (clippy + baseline Rust tests + JS lint + format check)
check-all: clippy test lint-js format-check

# ---------------------------------------------------------------------------
# Docker E2E testing (full stack with real isolate sandbox)
# ---------------------------------------------------------------------------

# Build and start the E2E Docker stack (server + 3 workers + PG + Redis)
e2e-docker-up *args:
    docker compose -f docker-compose.e2e.yml up --build -d {{args}}

# Stop the E2E Docker stack and remove volumes
e2e-docker-down:
    docker compose -f docker-compose.e2e.yml down -v

# Run E2E tests against a running Docker stack
e2e-docker-test *args:
    E2E_SERVER_URL=http://127.0.0.1:3000 E2E_DATABASE_URL=postgres://postgres:password@127.0.0.1:5433/broccoli cargo test -p server --test e2e -- --test-threads=8 {{args}}

# Run E2E tests in-process (testcontainers mode, no Docker stack needed)
e2e-local *args:
    E2E_SANDBOX_BACKEND=mock cargo test -p server --test e2e -- --test-threads=4 {{args}}

# Full E2E cycle: build images, start stack, run tests, tear down
e2e-docker *args:
    just e2e-docker-up
    just e2e-docker-test {{args}}; EXIT_CODE=$?; just e2e-docker-down; exit $EXIT_CODE

# Build Docker images with China mirrors (for mainland Chinese IPs)
e2e-docker-up-cn *args:
    docker compose -f docker-compose.e2e.yml build --build-arg USE_CN_MIRRORS=true
    docker compose -f docker-compose.e2e.yml up -d {{args}}

# ---------------------------------------------------------------------------
# Stress-test cross-builds (static musl binaries via `cross`)
# ---------------------------------------------------------------------------

# Cross-build static x86_64 Linux binary
stress-test-linux-x86_64:
    mkdir -p dist
    cross build --target x86_64-unknown-linux-musl --release -p stress-test
    cp target/x86_64-unknown-linux-musl/release/broccoli-stress-test \
       dist/broccoli-stress-test-linux-x86_64

# Cross-build static aarch64 Linux binary
stress-test-linux-aarch64:
    mkdir -p dist
    cross build --target aarch64-unknown-linux-musl --release -p stress-test
    cp target/aarch64-unknown-linux-musl/release/broccoli-stress-test \
       dist/broccoli-stress-test-linux-aarch64

# Build all stress-test release artifacts and write SHA256SUMS
stress-test-all: stress-test-linux-x86_64 stress-test-linux-aarch64
    cd dist && sha256sum broccoli-stress-test-linux-* > SHA256SUMS

# Run portability harness (requires prebuilt musl binaries; Linux only)
stress-test-portability:
    STRESS_TEST_PORTABILITY=1 cargo test -p stress-test --test portability
