# broccoli-dev-cli

`broccoli-dev-cli` installs the **`broccoli-dev`** command.

It is the command-line tool used to scaffold Broccoli plugins, build them
locally, and watch a plugin directory while pushing changes to a running
Broccoli server.

> Competing in a contest instead of writing plugins? That's the separate
> **`broccoli`** command (`packages/contestant-cli`).

## Install

```bash
cargo install broccoli-dev-cli
```

If you are working in the Broccoli monorepo and want the local checkout instead:

```bash
cargo install --path packages/dev-cli
```

## What it does

The CLI currently covers four jobs:

- Sign in to a Broccoli server with the device-code flow.
- Scaffold a new plugin directory with backend, frontend, or full templates.
- Build a plugin's backend WASM and frontend bundle from `plugin.toml`.
- Watch a plugin directory, rebuild on changes, and upload the result.

## Commands

### `broccoli-dev login`

Starts the device-code login flow against a Broccoli server.

```bash
broccoli-dev login
broccoli-dev login --server http://localhost:3000
broccoli-dev login --server http://localhost:3000 --token <TOKEN>
```

Without `--server`, it signs in to the server you signed in to last, then the
`server` in `config.toml`, then `http://localhost:3000`. `--token` saves an
access token (for example one copied from the web UI) instead of running the
device flow. On success, it prints where the credentials are stored:
`credentials.json` in the `broccoli` folder of the platform's config directory.
You can override them per command with `BROCCOLI_URL` and `BROCCOLI_TOKEN`, or
by passing `--server` and `--token` to `broccoli-dev plugin watch`.

### `broccoli-dev plugin new`

Creates a plugin scaffold from the built-in templates.

```bash
broccoli-dev plugin new my-plugin --full
broccoli-dev plugin new judge-tools --backend
broccoli-dev plugin new contest-banner --frontend
```

By default, `broccoli-dev plugin new` asks which kind of scaffold to generate.
Use `--backend`, `--frontend`, or `--full` to make the command non-interactive.

Useful flags:

- `-o, --output <DIR>` writes the scaffold somewhere other than `./<name>`.
- `--sdk <DIR>` takes the SDKs from a local directory laid out like a Broccoli
  checkout's `packages/` (`server-sdk/`, `broccoli-types/`, `web-sdk/`).

Inside a Broccoli checkout the scaffold depends on the checkout's SDK packages
by path, found automatically. Elsewhere, `--sdk` gives the same local
dependencies; without it, a backend depends on the published
`broccoli-server-sdk` at the version this CLI was built with, and a frontend
needs `--sdk` (with `web-sdk` built). Every scaffold is its own Cargo workspace
root, so it builds the same inside and outside a checkout.

### `broccoli-dev plugin build`

Builds the plugin described by the `plugin.toml` in the target directory.

```bash
broccoli-dev plugin build
broccoli-dev plugin build plugins/ioi
broccoli-dev plugin build plugins/ioi --install   # reinstall frontend deps first
broccoli-dev plugin build plugins/ioi --release
```

What gets built depends on the manifest:

- If the manifest has a `[server]` section, the CLI runs
  `cargo build --target wasm32-wasip1` and copies the built artifact to the
  manifest's configured entry path.
- If the manifest has a `[web]` section, the CLI runs a frontend build command
  and leaves the output in the manifest's configured web root.

If your frontend is not in the default location, add a `broccoli.dev.toml` next
to `plugin.toml`:

```toml
[build]
frontend_dir = "ui"
frontend_install_cmd = "pnpm install"
frontend_build_cmd = "pnpm build"
```

Without that file, the CLI tries to infer the frontend directory from
`[web].root`, then falls back to `frontend/`, or the plugin root if a
`package.json` is present. The default install command is `pnpm install`, so a
frontend that is a member of a pnpm workspace installs through that workspace's
lockfile; one that sits inside a workspace without being listed in its
`pnpm-workspace.yaml` gets `--ignore-workspace` and installs on its own.

### `broccoli-dev plugin watch`

Watches a plugin directory, rebuilds on changes, and uploads new bundles to a
Broccoli server.

```bash
broccoli-dev login --server http://localhost:3000
broccoli-dev plugin watch plugins/ioi --server http://localhost:3000
```

For backend changes, the CLI rebuilds the WASM module itself. For frontend
plugins, it starts a long-running dev command and watches the output directory
for fresh assets before packaging and uploading again. Each upload first asks
the server what it publishes under the plugin's id
(`/admin/plugins/upload/inspect`) and states that as the upload's `replaces`, as
the admin UI does, so it never replaces a change made meanwhile. Only edits
trigger a rebuild: reading files, metadata changes and the build's own outputs
(`target/`, the copied WASM entry, `Cargo.lock`) do not, so a failed build or
upload waits for the next edit.

Useful flags:

- `--server <URL>` overrides the saved server URL.
- `--token <TOKEN>` overrides the saved token.
- `--install` runs the frontend install command at startup even when
  `node_modules` exists.
- `--release` uses release builds for backend rebuilds.
- `--debounce <MS>` changes the file-watch debounce interval.

You can customize watch behavior with `broccoli.dev.toml`:

```toml
[watch]
ignore = ["*.log", "coverage/"]

[build]
frontend_dir = "ui"
frontend_install_cmd = "pnpm install"
frontend_build_cmd = "pnpm build"
frontend_dev_cmd = "pnpm dev"
```

Built-in ignores are always active for `target/`, `.git/`, and `node_modules/`.

## Typical workflow

Create a plugin, build it once, then switch to watch mode while the server is
running:

```bash
broccoli-dev plugin new my-plugin --full
broccoli-dev plugin build my-plugin
broccoli-dev login --server http://localhost:3000
broccoli-dev plugin watch my-plugin --server http://localhost:3000
```

## Notes

- The published crate name is `broccoli-dev-cli`, and the installed binary is
  `broccoli-dev`.
- `broccoli-dev plugin build` and `broccoli-dev plugin watch` both expect to
  find a `plugin.toml` in the target directory.

## License

MIT
