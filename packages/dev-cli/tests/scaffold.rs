//! `broccoli-dev plugin new` produces plugins that build: the backend checks
//! for the WASM target and the frontend type-checks against the web SDK,
//! both inside a Broccoli checkout and on their own with `--sdk`. Nothing is
//! fetched: cargo runs `--offline` against the crates the workspace already
//! uses, and the frontend resolves its packages from the checkout's install.

use std::path::{Path, PathBuf};
use std::process::Command;

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
    let plugin = dir.join("demo-plugin");
    let out = Command::new(env!("CARGO_BIN_EXE_broccoli-dev"))
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
    let cargo = PathBuf::from(env!("CARGO"));
    let rustc = cargo.with_file_name("rustc");
    let out = Command::new(&cargo)
        .args(["check", "--offline", "--target", "wasm32-wasip1"])
        .env("RUSTC", rustc)
        .env(
            "CARGO_TARGET_DIR",
            Path::new(env!("CARGO_TARGET_TMPDIR")).join("scaffold-target"),
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

#[cfg(unix)]
#[test]
fn a_standalone_frontend_type_checks_against_the_web_sdk() {
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
    let package_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(plugin.join("package.json")).unwrap())
            .unwrap();
    let spec = package_json["dependencies"]["@broccoli/web-sdk"]
        .as_str()
        .unwrap();
    let target = spec.strip_prefix("file:").expect("a file: dependency");
    assert_eq!(
        plugin.join(target).canonicalize().unwrap(),
        web_sdk.canonicalize().unwrap()
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
