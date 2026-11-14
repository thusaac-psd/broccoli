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
