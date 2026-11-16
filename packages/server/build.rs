use std::path::PathBuf;
use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=BROCCOLI_GIT_SHA={sha}");
    println!("cargo:rustc-env=BROCCOLI_SCHEMA_STAMP={}", schema_stamp());

    // Resolve the real .git directory so rerun-if-changed works regardless of
    // workspace layout, worktrees, or submodules. Cargo interprets relative
    // paths against the package manifest dir, so we need absolute (or
    // git-resolved) paths here.
    if let Some(git_dir) = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
    {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        println!("cargo:rerun-if-changed={git_dir}/packed-refs");
        println!("cargo:rerun-if-changed={git_dir}/refs");
    }

    if std::env::var("CARGO_FEATURE_BUNDLED_STRESS_TEST").is_ok() {
        check_embedded_binaries();
    }
}

/// Sources that define the database schema: the entities schema sync
/// creates, the migrations, and the trigger DDL installed at boot. Every
/// change to them that sync cannot apply needs a fresh database, so the stamp
/// covers all of them and any edit outside comments changes it.
const SCHEMA_SOURCES: &[&str] = &[
    "src/entity",
    "src/migration",
    "src/services/judging/notify.rs",
    "src/services/plugin_config_cache.rs",
];

/// FNV-1a over the schema sources with comment and blank lines removed, in
/// path order.
fn schema_stamp() -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for source in SCHEMA_SOURCES {
        collect_rs(&root.join(source), &mut files);
    }
    files.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for file in files {
        println!("cargo:rerun-if-changed={}", file.display());
        let text = std::fs::read_to_string(&file).expect("read a schema source");
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        let code = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for byte in rel.bytes().chain([0]).chain(code.bytes()).chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

fn collect_rs(path: &std::path::Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        println!("cargo:rerun-if-changed={}", path.display());
        for entry in std::fs::read_dir(path).expect("read a schema source dir") {
            collect_rs(&entry.expect("dir entry").path(), out);
        }
    } else if path.extension().is_some_and(|e| e == "rs") {
        out.push(path.to_path_buf());
    }
}

fn check_embedded_binaries() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("embedded")
        .join("stress-test");

    let required = [
        "linux-x86_64",
        "linux-aarch64",
        "windows-x86_64.exe",
        "macos-universal",
        "manifest.json",
    ];

    for name in required {
        let path = dir.join(name);
        if !path.exists() {
            eprintln!(
                "\nerror: feature `bundled-stress-test` requires {} to exist.\n\
                 Run scripts/fetch-stress-test-binaries.sh <version> to fetch them \
                 from GitHub Releases, or unset the feature.\n",
                path.display()
            );
            std::process::exit(1);
        }
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
