---
title: Getting started
sidebar_label: Getting started
sidebar_position: 1
---

# Getting started

A Broccoli plugin is a WebAssembly module that runs inside the Broccoli server,
plus an optional React bundle that runs in the web frontend. The backend module
reaches the platform through host functions for the database, configuration, and
logging, gated by the permissions it declares. This page builds a plugin end to
end, using the shipped `cooldown` plugin as the worked example.

## The contract

Your backend code runs as a sandboxed WASM module. It does not open sockets or
touch the filesystem. It talks to the host through declared host functions, and
its HTTP handlers return a JSON value, never a file or a stream. That is why a
plugin coordinates and stores data while anything binary, like a rendered PDF,
has to live in a separate native client. Knowing this up front saves you from
designing against the grain.

A plugin can add two kinds of behavior:

- Backend routes and hooks, written in Rust and compiled to WASM.
- Frontend components, written in React and mounted into named UI slots.

## Prerequisites

- Rust with the `wasm32-wasip1` target. A plugin scaffolded on its own gets a
  `rust-toolchain.toml` that selects stable Rust with that target. A plugin
  scaffolded inside a project that already pins a toolchain, such as the
  Broccoli repository, builds with that project's pin instead.
- The Broccoli CLI. The crate is `broccoli-dev-cli` and the installed binary is
  `broccoli-dev`.
- `pnpm`, if your plugin has a frontend.
- A running Broccoli server to upload to.

Install the CLI:

```bash
cargo install broccoli-dev-cli
```

## Scaffold a plugin

```bash
broccoli-dev plugin new my-plugin --full
```

Use `--backend`, `--frontend`, or `--full` to pick what gets generated. Without
one of those flags the command asks. The scaffold lands in `./my-plugin`, or
pass `-o <DIR>` to write it elsewhere.

The scaffold depends on the Broccoli SDKs. Created inside a Broccoli checkout,
it uses the checkout's `packages/` directory, so it builds against the code
next to it. Elsewhere, pass `--sdk <DIR>` with the `packages/` directory of a
checkout to get the same local dependencies, which build without fetching the
SDKs from anywhere. Without `--sdk`, a backend depends on the published
`broccoli-server-sdk` crate at the version the CLI was built with; a frontend
outside a checkout needs `--sdk`, with `packages/web-sdk` built
(`pnpm --filter @broccoli/web-sdk build`).

## Anatomy

Three files define a backend plugin.

### plugin.toml

The manifest. It names the plugin, declares what the backend may do, and maps
events and routes to exported functions. Here is the `cooldown` manifest, trimmed
to its backend:

```toml
name = "cooldown"
version = "0.1.0"
description = "Submission cooldown timer"

[server]
entry = "cooldown_plugin.wasm"
permissions = ["logger", "sql", "config:read"]

[[server.hooks]]
topic = "before_submission"
function = "check_cooldown"
scope = "resource"

[[server.routes]]
method = "GET"
path = "/api/plugins/cooldown/problems/{problem_id}/status"
handler = "get_cooldown_status_standalone"
```

- `entry` is the WASM filename the build produces.
- `permissions` gate host access. Without `sql` the database host functions are
  not available, and so on.
- With `sql`, the plugin's tables land in its own schema, and host tables are
  read-only to it. Contestant source, test data, program output and message
  bodies stay unreadable unless `[server.sql] read` names the columns, for
  example `read = ["submission.files"]`; the admin sees them when installing
  the plugin. See [Plugin database access](../internals/plugin-database.md).
- A hook subscribes a function to a platform event, here `before_submission`.
  With `scope = "resource"` (the default) it runs only where the plugin
  applies: in a contest or problem whose settings turn the plugin on, and in
  every contest of a type the plugin itself registered. With
  `scope = "global"` it runs for every submission on the server.
- A route maps an HTTP method and path to an exported function. Path parameters
  like `{problem_id}` are read inside the handler.
- An optional `[bundle]` section says how release bundles ship the plugin.
  `mode = "default"` (the default) ships it and loads it. `mode = "optional"`
  ships it in `plugins-available/`, where operators enable it with
  `enable-plugin.sh`; use this for plugins only some events need.
  `mode = "none"` never ships it, for test fixtures.

### Cargo.toml

```toml
[package]
name = "cooldown-plugin"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
broccoli-server-sdk = { path = "../../packages/server-sdk", features = ["guest"] }
extism-pdk = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

The `cdylib` crate type produces the WASM module. The `guest` feature on
`broccoli-server-sdk` turns on the host function wrappers your code calls.

### src/lib.rs

Exported functions are marked with `#[plugin_fn]`. A route handler takes the
request as a string and returns a JSON string. `run_api_handler` decodes the
request and hands you a typed `Host` and `PluginHttpRequest`:

```rust
use broccoli_server_sdk::prelude::*;
use extism_pdk::{plugin_fn, FnResult};

#[plugin_fn]
pub fn get_cooldown_status_standalone(input: String) -> FnResult<String> {
    run_api_handler(&input, handle_status)
}

fn handle_status(
    host: &Host,
    req: &PluginHttpRequest,
) -> Result<PluginHttpResponse, ApiError> {
    let user_id = req
        .require_user_id()
        .map_err(|_| PluginHttpResponse::error(401, "Authentication required"))?;
    let problem_id: i32 = req.param("problem_id")?;

    let eff = host.config.get_effective("cooldown", problem_id, None)?;

    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(serde_json::json!({
            "enabled": eff.is_enabled,
            "user_id": user_id,
        })),
    })
}
```

The response `body` is a `serde_json` value. That is the JSON-only contract in
practice.

A hook returns a small JSON decision instead of an HTTP response. The cooldown
hook reads its config, checks the last submission time through `host.db`, and
returns either `{"action": "pass"}` or a rejection:

```rust
#[plugin_fn]
pub fn check_cooldown(input: String) -> FnResult<String> {
    let host = Host::new();
    let event: BeforeSubmissionEvent = serde_json::from_str(&input)?;

    let eff = host
        .config
        .get_effective("cooldown", event.problem_id, event.contest_id)?;
    if !eff.is_enabled {
        return Ok(serde_json::to_string(&serde_json::json!({"action": "pass"}))?);
    }

    // ... query host.db for seconds since the last submission ...

    Ok(serde_json::to_string(&serde_json::json!({"action": "pass"}))?)
}
```

## Build and install

```bash
broccoli-dev plugin build my-plugin
```

Because the manifest has a `[server]` section, the CLI runs
`cargo build --target wasm32-wasip1` and copies the artifact to the `entry` path
inside the plugin directory. Add `--release` for an optimized module. If the
manifest has a `[web]` section, the CLI also runs the frontend build, first
installing the frontend's dependencies when `node_modules` is missing;
`--install` runs the install command even when it exists. A server loads the
plugin from its plugins directory, or from an upload, which `plugin watch` does
for you.

## Iterate against a running server

Sign in once, then watch the directory. The CLI rebuilds on every edit and
uploads the new bundle, replacing what the server publishes under the plugin's
id. A failed build or upload waits for your next edit instead of retrying:

```bash
broccoli-dev login --server http://localhost:3000
broccoli-dev plugin watch my-plugin --server http://localhost:3000
```

`broccoli-dev login` stores credentials in `credentials.json` in the `broccoli`
folder of your platform's config directory (`~/.config/broccoli` on Linux) and
prints the exact path. On a machine without a browser, save a token copied from
the web UI with `broccoli-dev login --server <URL> --token <TOKEN>`. You can
override the saved login with `BROCCOLI_URL` and `BROCCOLI_TOKEN`, or with
`--server` and `--token` on `plugin watch`. Uploading needs the `plugin:manage`
permission and a server with plugin uploads switched on.

## Where to go next

- The host surface lives behind `host`. Use `host.db` with the `Params` builder
  for parameterized SQL, `host.config.get_effective` for layered configuration,
  and `host.log` for logging.
- Declare a `[config.<plugin>]` block in `plugin.toml` to get a settings form in
  the admin UI, scoped to a problem, a contest, or a contest problem.
- Mount frontend components into named slots with the web SDK. See the
  `[web]` and `[[web.slots]]` sections of a full plugin manifest.
