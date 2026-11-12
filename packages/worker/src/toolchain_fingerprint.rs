use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{info, warn};

#[derive(Clone, Debug)]
pub struct Probe {
    pub name: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
}

pub fn default_probes() -> &'static [Probe] {
    &[
        Probe {
            name: "gcc",
            command: "gcc",
            args: &["--version"],
        },
        Probe {
            name: "g++",
            command: "g++",
            args: &["--version"],
        },
        Probe {
            name: "clang",
            command: "clang",
            args: &["--version"],
        },
        Probe {
            name: "clang++",
            command: "clang++",
            args: &["--version"],
        },
        Probe {
            name: "python3",
            command: "python3",
            args: &["--version"],
        },
        Probe {
            name: "java",
            command: "java",
            args: &["-version"],
        },
        Probe {
            name: "javac",
            command: "javac",
            args: &["-version"],
        },
    ]
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The toolchains this worker compiles with, as far as its probes could tell.
/// Part of every compile-cache key, so two workers share compiled artifacts
/// only when their toolchains match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolchainFingerprint {
    /// Every probe either reported a version or found its toolchain absent.
    Known(String),
    /// A probe failed in a way that says nothing about the toolchain (it
    /// timed out, exited non-zero, or could not be started for a reason other
    /// than the binary being absent). No cache key built from it can be
    /// trusted, so this worker compiles without the cache.
    Unreliable,
}

impl ToolchainFingerprint {
    /// The fingerprint to key the compile cache with, if it is trustworthy.
    pub fn cache_key_part(&self) -> Option<&str> {
        match self {
            Self::Known(fingerprint) => Some(fingerprint),
            Self::Unreliable => None,
        }
    }
}

impl std::fmt::Display for ToolchainFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Known(fingerprint) => f.write_str(fingerprint),
            Self::Unreliable => f.write_str("unreliable"),
        }
    }
}

/// What one probe found.
enum ProbeOutcome {
    Version(String),
    Absent,
    Failed,
}

pub async fn compute(probes: &[Probe]) -> ToolchainFingerprint {
    let mut entries: Vec<(&'static str, String)> = Vec::with_capacity(probes.len());
    for probe in probes {
        let entry = match probe_one(probe).await {
            ProbeOutcome::Version(version) => version,
            ProbeOutcome::Absent => "absent".to_string(),
            ProbeOutcome::Failed => return ToolchainFingerprint::Unreliable,
        };
        entries.push((probe.name, entry));
    }
    entries.sort_by_key(|(name, _)| *name);

    let mut hasher = Sha256::new();
    for (name, output) in &entries {
        hasher.update(name.as_bytes());
        hasher.update(b"\0");
        hasher.update(output.as_bytes());
        hasher.update(b"\0");
    }
    ToolchainFingerprint::Known(hex::encode(hasher.finalize()))
}

async fn probe_one(probe: &Probe) -> ProbeOutcome {
    let mut cmd = Command::new(probe.command);
    cmd.args(probe.args);
    cmd.kill_on_drop(true);

    match timeout(PROBE_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            let mut combined = Vec::with_capacity(output.stdout.len() + output.stderr.len());
            combined.extend_from_slice(&output.stdout);
            combined.extend_from_slice(&output.stderr);
            let text = String::from_utf8_lossy(&combined).trim().to_string();
            info!(probe = probe.name, version = %text, "toolchain probe succeeded");
            ProbeOutcome::Version(text)
        }
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            warn!(
                probe = probe.name,
                exit_code = output.status.code(),
                stderr = %stderr,
                "toolchain probe exited non-zero; compile cache disabled"
            );
            ProbeOutcome::Failed
        }
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            info!(probe = probe.name, "toolchain not installed");
            ProbeOutcome::Absent
        }
        Ok(Err(e)) => {
            warn!(probe = probe.name, error = %e, "toolchain probe failed to spawn; compile cache disabled");
            ProbeOutcome::Failed
        }
        Err(_) => {
            warn!(
                probe = probe.name,
                "toolchain probe timed out; compile cache disabled"
            );
            ProbeOutcome::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn compute_returns_nonempty_for_empty_probe_list() {
        let fp = compute(&[]).await;
        assert_eq!(fp.cache_key_part().map(str::len), Some(64));
    }

    #[tokio::test]
    async fn compute_is_deterministic_across_calls() {
        let probes = [
            Probe {
                name: "echo-a",
                command: "/bin/echo",
                args: &["a"],
            },
            Probe {
                name: "echo-b",
                command: "/bin/echo",
                args: &["b"],
            },
        ];
        let fp1 = compute(&probes).await;
        let fp2 = compute(&probes).await;
        assert_eq!(fp1, fp2);
    }

    #[tokio::test]
    async fn compute_is_order_independent() {
        let p1 = [
            Probe {
                name: "a",
                command: "/bin/echo",
                args: &["1"],
            },
            Probe {
                name: "b",
                command: "/bin/echo",
                args: &["2"],
            },
        ];
        let p2 = [
            Probe {
                name: "b",
                command: "/bin/echo",
                args: &["2"],
            },
            Probe {
                name: "a",
                command: "/bin/echo",
                args: &["1"],
            },
        ];
        assert_eq!(compute(&p1).await, compute(&p2).await);
    }

    #[tokio::test]
    async fn compute_differs_for_different_outputs() {
        let p1 = [Probe {
            name: "x",
            command: "/bin/echo",
            args: &["v1"],
        }];
        let p2 = [Probe {
            name: "x",
            command: "/bin/echo",
            args: &["v2"],
        }];
        assert_ne!(compute(&p1).await, compute(&p2).await);
    }

    #[tokio::test]
    async fn missing_binaries_give_the_same_known_fingerprint() {
        let missing = [Probe {
            name: "x",
            command: "/no-such-binary-a",
            args: &[],
        }];
        let also_missing = [Probe {
            name: "x",
            command: "/no-such-binary-b",
            args: &[],
        }];
        let fp = compute(&missing).await;
        assert!(fp.cache_key_part().is_some());
        assert_eq!(fp, compute(&also_missing).await);
    }

    #[tokio::test]
    async fn a_failing_probe_makes_the_fingerprint_unreliable() {
        let probes = [
            Probe {
                name: "ok",
                command: "/bin/echo",
                args: &["1"],
            },
            Probe {
                name: "broken",
                command: "/bin/false",
                args: &[],
            },
        ];
        let fp = compute(&probes).await;
        assert_eq!(fp, ToolchainFingerprint::Unreliable);
        assert_eq!(fp.cache_key_part(), None);
    }
}
