//! `broccoli-dev plugin new` produces plugins that build: the backend checks
//! for the WASM target and the frontend type-checks against the web SDK,
//! inside a Broccoli checkout, on their own with `--sdk`, and against a
//! release bundle's staged `sdk/` found from where `broccoli-dev` runs.
//! Nothing is fetched: cargo runs `--offline`, against the crates the
//! workspace already uses or, for the bundle, against its `vendor/` alone
//! with an empty `CARGO_HOME`, and the frontend resolves its packages from
//! the checkout's install. The cargo cache must hold the workspace's crates
//! (`cargo fetch`) and the checkout's JavaScript packages must be installed
//! (`pnpm install`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Held by every test that reads or rebuilds `packages/web-sdk/dist`:
/// staging a bundle rebuilds it.
static WEB_SDK_DIST: Mutex<()> = Mutex::new(());

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

/// A fresh directory under the repository's `target/`, so it is inside the
/// checkout.
fn scratch_in_checkout(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("scaffold-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn scaffold(dir: &Path, kind: &str, extra: &[&str]) -> PathBuf {
    scaffold_with(
        Path::new(env!("CARGO_BIN_EXE_broccoli-dev")),
        dir,
        kind,
        extra,
    )
}

fn scaffold_with(exe: &Path, dir: &Path, kind: &str, extra: &[&str]) -> PathBuf {
    let plugin = dir.join("demo-plugin");
    let out = Command::new(exe)
        .args(["plugin", "new", "demo-plugin", kind, "-o"])
        .arg(&plugin)
        .args(extra)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "plugin new failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    plugin
}

/// `cargo check` for the WASM target with the toolchain running this test,
/// whatever `rust-toolchain.toml` the scaffold has, and offline.
fn cargo_check_wasm(plugin: &Path) {
    cargo_check_wasm_with(plugin, Command::new(env!("CARGO")), "scaffold-target");
}

fn cargo_check_wasm_with(plugin: &Path, mut cargo: Command, target_dir: &str) {
    let rustc = Path::new(env!("CARGO")).with_file_name("rustc");
    let out = cargo
        .args(["check", "--offline", "--target", "wasm32-wasip1"])
        .env("RUSTC", rustc)
        .env(
            "CARGO_TARGET_DIR",
            Path::new(env!("CARGO_TARGET_TMPDIR")).join(target_dir),
        )
        .current_dir(plugin)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the scaffolded backend does not build:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_backend_scaffolded_in_a_checkout_builds_against_its_sdk() {
    let plugin = scaffold(&scratch_in_checkout("in-checkout"), "--backend", &[]);
    let cargo_toml = std::fs::read_to_string(plugin.join("Cargo.toml")).unwrap();
    assert!(
        cargo_toml.contains(r#"path = "../../../../packages/server-sdk""#),
        "{cargo_toml}"
    );
    cargo_check_wasm(&plugin);
}

#[test]
fn a_standalone_backend_builds_against_the_sdk_directory_it_was_given() {
    let dir = tempfile::tempdir().unwrap();
    let packages = repo_root().join("packages");
    let plugin = scaffold(
        dir.path(),
        "--backend",
        &["--sdk", packages.to_str().unwrap()],
    );
    assert!(plugin.join("rust-toolchain.toml").is_file());
    cargo_check_wasm(&plugin);
}

/// The `dependencies`, `devDependencies` and `peerDependencies` of a
/// `package.json` whose version is one only the pnpm workspace that defines
/// it resolves.
fn workspace_only_specs(package_json: &serde_json::Value) -> Vec<String> {
    ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .filter_map(|field| package_json[field].as_object())
        .flatten()
        .filter(|(_, spec)| {
            spec.as_str()
                .is_some_and(|s| s.starts_with("catalog:") || s.starts_with("workspace:"))
        })
        .map(|(name, spec)| format!("{name}@{spec}"))
        .collect()
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The `package.json` inside the npm tarball at `tgz`.
fn packed_package_json(tgz: &Path) -> serde_json::Value {
    let gz = flate2::read::GzDecoder::new(std::fs::File::open(tgz).unwrap());
    let mut archive = tar::Archive::new(gz);
    let entry = archive
        .entries()
        .unwrap()
        .map(Result::unwrap)
        .find(|e| e.path().unwrap() == Path::new("package/package.json"))
        .expect("the tarball holds package/package.json");
    serde_json::from_reader(entry).unwrap()
}

#[cfg(unix)]
#[test]
fn a_standalone_frontend_depends_on_a_packed_web_sdk_and_type_checks_against_it() {
    let _dist = WEB_SDK_DIST.lock().unwrap_or_else(|e| e.into_inner());
    let root = repo_root();
    let web_sdk = root.join("packages/web-sdk");
    let deps = web_sdk.join("node_modules");
    assert!(
        web_sdk.join("dist/i18n/index.d.ts").is_file() && deps.join("typescript").is_dir(),
        "run `pnpm install` and `pnpm --filter @broccoli/web-sdk build` first"
    );

    let dir = tempfile::tempdir().unwrap();
    let packages = root.join("packages");
    let plugin = scaffold(
        dir.path(),
        "--frontend",
        &["--sdk", packages.to_str().unwrap()],
    );
    let package_json = read_json(&plugin.join("package.json"));
    let spec = package_json["dependencies"]["@broccoli/web-sdk"]
        .as_str()
        .unwrap();
    let tarball = spec.strip_prefix("file:").expect("a file: dependency");
    assert!(!tarball.contains('/'), "packed into the plugin: {spec}");
    let packed = packed_package_json(&plugin.join(tarball));
    assert_eq!(packed["name"], "@broccoli/web-sdk");
    assert_eq!(
        workspace_only_specs(&packed),
        Vec::<String>::new(),
        "pnpm cannot install these outside the checkout's workspace"
    );
    assert!(package_json["scripts"].get("preinstall").is_none());

    // What `pnpm install` would link, taken from the checkout's install.
    let modules = plugin.join("node_modules");
    std::fs::create_dir_all(modules.join("@broccoli")).unwrap();
    std::fs::create_dir_all(modules.join("@types")).unwrap();
    let link = |from: &Path, to: &Path| std::os::unix::fs::symlink(from, to).unwrap();
    link(&web_sdk, &modules.join("@broccoli/web-sdk"));
    link(&deps.join("react"), &modules.join("react"));
    link(&deps.join("@types/react"), &modules.join("@types/react"));

    let out = Command::new("node")
        .arg(deps.join("typescript/bin/tsc"))
        .args(["--noEmit", "-p", "tsconfig.json"])
        .current_dir(&plugin)
        .output()
        .expect("node is installed");
    assert!(
        out.status.success(),
        "the scaffolded frontend does not type-check:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// A copy of the air-gap bundle's plugin-author tree: the SDKs as
/// `scripts/stage-sdk.sh` stages them in `sdk/`, and `broccoli-dev` in
/// `cli/`.
#[cfg(unix)]
fn stage_bundle(bundle: &Path) -> PathBuf {
    let out = Command::new(repo_root().join("scripts/stage-sdk.sh"))
        .arg(bundle)
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "staging the SDKs failed; run `cargo fetch` and `pnpm install` first:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let exe = bundle.join("cli/broccoli-dev");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_broccoli-dev"), &exe).unwrap();
    exe
}

#[cfg(unix)]
#[test]
fn a_plugin_scaffolded_by_a_bundled_cli_builds_offline_against_the_bundle_sdk() {
    let bundle = tempfile::tempdir().unwrap();
    let exe = {
        let _dist = WEB_SDK_DIST.lock().unwrap_or_else(|e| e.into_inner());
        stage_bundle(bundle.path())
    };
    let sdk = bundle.path().canonicalize().unwrap().join("sdk");

    let elsewhere = tempfile::tempdir().unwrap();
    let plugin = scaffold_with(&exe, elsewhere.path(), "--full", &[]);

    let cargo_toml: toml::Table =
        toml::from_str(&std::fs::read_to_string(plugin.join("Cargo.toml")).unwrap()).unwrap();
    let sdk_path = cargo_toml["dependencies"]["broccoli-server-sdk"]["path"]
        .as_str()
        .expect("a path dependency");
    assert_eq!(
        plugin.join(sdk_path).canonicalize().unwrap(),
        sdk.join("server-sdk")
    );

    let frontend = plugin.join("frontend");
    let spec = read_json(&frontend.join("package.json"))["dependencies"]["@broccoli/web-sdk"]
        .as_str()
        .unwrap()
        .to_owned();
    let web_sdk = frontend.join(spec.strip_prefix("file:").expect("a file: dependency"));
    assert_eq!(web_sdk.canonicalize().unwrap(), sdk.join("web-sdk"));
    assert_eq!(
        workspace_only_specs(&read_json(&web_sdk.join("package.json"))),
        Vec::<String>::new()
    );

    // Only the bundle: no cargo cache, crates.io replaced by sdk/vendor.
    let cargo_home = tempfile::tempdir().unwrap();
    let mut cargo = Command::new(env!("CARGO"));
    cargo
        .env("CARGO_HOME", cargo_home.path())
        .arg("--config")
        .arg(sdk.join(".cargo/config.toml"));
    cargo_check_wasm_with(&plugin, cargo, "scaffold-target-bundle");
}
