//! Server-bundled stress-test binary downloads.
//!
//! Compiled only when the `bundled-stress-test` cargo feature is enabled.

use axum::{
    Router,
    body::Body,
    extract::Path,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use sha2::{Digest, Sha256};

use crate::state::AppState;

const LINUX_X86_64: &[u8] = include_bytes!("../../embedded/stress-test/linux-x86_64");
const LINUX_AARCH64: &[u8] = include_bytes!("../../embedded/stress-test/linux-aarch64");
const WINDOWS_X86_64: &[u8] = include_bytes!("../../embedded/stress-test/windows-x86_64.exe");
const MACOS_UNIVERSAL: &[u8] = include_bytes!("../../embedded/stress-test/macos-universal");
const MANIFEST_JSON: &[u8] = include_bytes!("../../embedded/stress-test/manifest.json");

/// Returns `(bytes, download_filename)` for a known platform identifier, or `None`.
fn binary_for(platform: &str) -> Option<(&'static [u8], &'static str)> {
    match platform {
        "linux-x86_64" => Some((LINUX_X86_64, "broccoli-stress-test-linux-x86_64")),
        "linux-aarch64" => Some((LINUX_AARCH64, "broccoli-stress-test-linux-aarch64")),
        "windows-x86_64" => Some((WINDOWS_X86_64, "broccoli-stress-test-windows-x86_64.exe")),
        "macos-universal" => Some((MACOS_UNIVERSAL, "broccoli-stress-test-macos-universal")),
        _ => None,
    }
}

const PLATFORMS: &[&str] = &[
    "linux-x86_64",
    "linux-aarch64",
    "windows-x86_64",
    "macos-universal",
];

pub fn router() -> Router<AppState> {
    // axum/matchit forbids mixing a parameter with a literal in the same path
    // segment (`{platform}.sha256` is rejected). Instead we accept a single
    // `{file}` segment and dispatch on the optional `.sha256` suffix in code.
    Router::new()
        .route("/downloads/stress-test/{file}", get(serve))
        .route("/downloads/manifest.json", get(serve_manifest))
        .route("/downloads", get(serve_discovery))
        .route("/downloads/help", get(serve_help))
}

fn detect_platform(user_agent: &str) -> Option<&'static str> {
    let ua = user_agent.to_ascii_lowercase();
    if ua.contains("windows") {
        Some("windows-x86_64")
    } else if ua.contains("mac os") || ua.contains("macintosh") {
        Some("macos-universal")
    } else if ua.contains("linux") {
        if ua.contains("aarch64") || ua.contains("arm64") {
            Some("linux-aarch64")
        } else {
            Some("linux-x86_64")
        }
    } else {
        None
    }
}

async fn serve_discovery(headers: axum::http::HeaderMap) -> Response {
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let recommended = detect_platform(ua);
    let html = render_discovery_html(env!("CARGO_PKG_VERSION"), recommended);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .unwrap()
}

fn render_discovery_html(version: &str, recommended: Option<&str>) -> String {
    let mut rows = String::new();
    for &p in PLATFORMS {
        let badge = if Some(p) == recommended {
            r#" <span style="background:#ffd700;padding:2px 6px;border-radius:3px;font-size:0.85em;">Recommended for your platform</span>"#
        } else {
            ""
        };
        rows.push_str(&format!(
            r#"<tr>
              <td><code>{p}</code>{badge}</td>
              <td><a href="/downloads/stress-test/{p}">download</a></td>
              <td><a href="/downloads/stress-test/{p}.sha256">sha256</a></td>
            </tr>"#
        ));
    }
    format!(
        r#"<!doctype html>
<html><head>
  <meta charset="utf-8">
  <title>Broccoli Stress Test - Downloads</title>
  <style>
    body {{ font-family: system-ui, sans-serif; max-width: 720px; margin: 2em auto; padding: 0 1em; }}
    table {{ border-collapse: collapse; width: 100%; margin: 1em 0; }}
    td, th {{ padding: 0.5em; border-bottom: 1px solid #ddd; text-align: left; }}
    code {{ background: #f4f4f4; padding: 2px 4px; border-radius: 3px; }}
  </style>
</head><body>
  <h1>Broccoli Stress Test</h1>
  <p>Server version: <code>{version}</code></p>
  <p>Pick the binary for your platform. After download, see
     <a href="/downloads/help">trouble running?</a> for OS-specific notes.</p>
  <table>
    <thead><tr><th>Platform</th><th>Binary</th><th>Checksum</th></tr></thead>
    <tbody>{rows}</tbody>
  </table>
  <p>Machine-readable manifest: <a href="/downloads/manifest.json">/downloads/manifest.json</a></p>
</body></html>
"#
    )
}

async fn serve_help() -> Response {
    let html = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Trouble Running Broccoli Stress Test</title>
<style>body{font-family:system-ui,sans-serif;max-width:720px;margin:2em auto;padding:0 1em;}
code,pre{background:#f4f4f4;padding:2px 4px;border-radius:3px;}
pre{padding:1em;overflow-x:auto;}</style></head><body>
<h1>Trouble running?</h1>
<p>The binaries are not code-signed in v1. Each OS has a one-line workaround:</p>

<h2>macOS</h2>
<p>If macOS says "cannot be opened because the developer cannot be verified":</p>
<pre>xattr -d com.apple.quarantine ./broccoli-stress-test-macos-universal</pre>
<p>Or right-click the binary in Finder, choose Open, then click Open in the dialog.</p>

<h2>Windows</h2>
<p>If SmartScreen warns "Microsoft Defender prevented an unrecognized app from starting":</p>
<ol><li>Click <strong>More info</strong>.</li>
<li>Click <strong>Run anyway</strong>.</li></ol>
<p>Or in PowerShell: <code>Unblock-File .\broccoli-stress-test-windows-x86_64.exe</code></p>

<h2>Linux</h2>
<pre>chmod +x ./broccoli-stress-test-linux-x86_64
./broccoli-stress-test-linux-x86_64 --help</pre>

<p><a href="/downloads">&larr; back to downloads</a></p>
</body></html>
"#;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .unwrap()
}

async fn serve(Path(file): Path<String>) -> Response {
    if let Some(platform) = file.strip_suffix(".sha256") {
        serve_sha256(platform)
    } else {
        serve_binary(&file)
    }
}

fn serve_binary(platform: &str) -> Response {
    let Some((bytes, filename)) = binary_for(platform) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let server_version = env!("CARGO_PKG_VERSION");
    let etag = format!("\"{server_version}-{platform}\"");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .header(header::ETAG, HeaderValue::from_str(&etag).unwrap())
        .body(Body::from(bytes))
        .unwrap()
}

fn serve_sha256(platform: &str) -> Response {
    let Some((bytes, filename)) = binary_for(platform) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hex::encode(hasher.finalize());
    let body = format!("{digest}  {filename}\n");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(body))
        .unwrap()
}

async fn serve_manifest() -> Response {
    let manifest = rewrite_manifest_urls(MANIFEST_JSON);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(manifest))
        .unwrap()
}

/// Rewrite the GitHub Releases URLs in the embedded manifest to relative server paths,
/// so air-gapped lab clients never get pointed back at github.com.
fn rewrite_manifest_urls(raw: &[u8]) -> Vec<u8> {
    let mut value: serde_json::Value =
        serde_json::from_slice(raw).expect("embedded manifest.json must be valid JSON");

    if let Some(platforms) = value.get_mut("platforms").and_then(|p| p.as_object_mut()) {
        for (platform, info) in platforms.iter_mut() {
            if let Some(obj) = info.as_object_mut() {
                obj.insert(
                    "url".into(),
                    serde_json::Value::String(format!("/downloads/stress-test/{platform}")),
                );
            }
        }
    }

    serde_json::to_vec_pretty(&value).expect("re-serialize manifest")
}
