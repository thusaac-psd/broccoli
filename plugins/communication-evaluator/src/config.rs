use std::collections::HashSet;

use broccoli_server_sdk::evaluator::SandboxConfig;
use broccoli_server_sdk::types::{
    ConfigViolation, EvaluationTimeoutBudget, ResourceLimits, RunSpec,
};
use serde::Deserialize;

/// Kernel pipe buffer size requested for each contestant FIFO, in bytes.
pub const FIFO_BUFFER_SIZE: usize = 8192;

/// A single manager source file entry (filename + content hash).
#[derive(Debug, Clone, Deserialize)]
pub struct ManagerSourceEntry {
    pub filename: String,
    pub hash: String,
}

/// Per-problem communication config: the `communication` config namespace.
/// Its defaults and bounds (`num_processes` from 1 to 10, for one) live in the
/// namespace's schema in `plugin.toml`; the host fills the defaults in, so
/// every field is present here.
#[derive(Debug, Clone, Deserialize)]
pub struct CommConfig {
    pub num_processes: u32,
    pub communication_mode: CommunicationMode,
    pub manager_language: String,
    /// Manager source files. Each entry maps a filename to a blob hash.
    /// The first entry is the primary source file used for language resolution.
    pub manager_sources: Vec<ManagerSourceEntry>,
    pub manager_time_limit_s: f64,
    pub manager_memory_limit_kb: u32,
}

impl CommConfig {
    /// What the `communication` namespace refuses beyond its schema: two
    /// manager sources with one filename, of which the sandbox would keep
    /// only one.
    pub fn violations(&self) -> Vec<ConfigViolation> {
        let mut seen = HashSet::new();
        self.manager_sources
            .iter()
            .enumerate()
            .filter(|(_, source)| !seen.insert(source.filename.as_str()))
            .map(|(i, source)| {
                ConfigViolation::new(
                    format!("manager_sources[{i}].filename"),
                    format!(
                        "another manager source is already named '{}'",
                        source.filename
                    ),
                )
                .key("communication-evaluator.config.managerFilenameTaken")
                .param("filename", source.filename.as_str())
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The configs with nothing stored: the schema's defaults.
    pub(crate) fn default_sandbox() -> SandboxConfig {
        SandboxConfig::load(&broccoli_server_sdk::Host::mock()).unwrap()
    }

    pub(crate) fn default_comm() -> CommConfig {
        broccoli_server_sdk::Host::mock()
            .config
            .get_problem(1, "communication")
            .and_then(|c| c.parse())
            .unwrap()
    }

    fn source(filename: &str) -> ManagerSourceEntry {
        ManagerSourceEntry {
            filename: filename.into(),
            hash: "h".into(),
        }
    }

    #[test]
    fn manager_sources_sharing_a_filename_are_refused() {
        let config = CommConfig {
            manager_sources: vec![source("manager.cpp"), source("a.h"), source("manager.cpp")],
            ..default_comm()
        };
        let paths: Vec<_> = config.violations().into_iter().map(|v| v.path).collect();
        assert_eq!(paths, ["manager_sources[2].filename"]);
        let distinct = CommConfig {
            manager_sources: vec![source("manager.cpp"), source("a.h")],
            ..default_comm()
        };
        assert_eq!(distinct.violations(), []);
    }

    #[test]
    fn result_timeout_accounts_for_compile_run_and_manager_budget() {
        let config = SandboxConfig {
            compile_time_limit_s: 120.0,
            compile_wall_time_multiplier: 3.0,
            exec_wall_time_multiplier: 5.0,
            ..default_sandbox()
        };
        let without_manager = result_timeout_ms(&config, 300_000, 3, 0.0);
        assert!(without_manager > config.result_timeout_ms);
        assert!(result_timeout_ms(&config, 300_000, 3, 120.0) > without_manager);
    }

    #[test]
    fn the_manager_gets_two_fifos_per_contestant_and_its_languages_floor() {
        let comm = CommConfig {
            num_processes: 40,
            ..default_comm()
        };
        let manager = RunSpec {
            command: vec!["java".into(), "Manager".into()],
            extra_files: vec![],
            min_process_limit: Some(64),
        };
        let limits = manager_limits(&default_sandbox(), &manager, &comm);
        assert_eq!(limits.open_files_limit, Some(88));
        assert_eq!(limits.process_limit, Some(64));
        assert_eq!(limits.time_limit, Some(comm.manager_time_limit_s));
        assert_eq!(limits.memory_limit, Some(comm.manager_memory_limit_kb));
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommunicationMode {
    Redirect,
    FifoArgs,
}

/// The limits the manager runs under: a run of the manager's language with
/// the manager's own time and memory, and file descriptors for its 2 FIFOs
/// per contestant beside its standard and input files, however low the
/// namespace's cap.
pub fn manager_limits(
    sandbox: &SandboxConfig,
    manager: &RunSpec,
    comm: &CommConfig,
) -> ResourceLimits {
    let fds = 2 * comm.num_processes + 8;
    let mut limits = sandbox.run_limits(
        manager,
        comm.manager_time_limit_s,
        comm.manager_memory_limit_kb,
    );
    limits.open_files_limit = Some(sandbox.exec_open_files_limit.max(fds).max(64));
    limits
}

/// How long a case may take: the compiles, one contestant run and the
/// manager's run.
pub fn result_timeout_ms(
    sandbox: &SandboxConfig,
    time_limit_ms: i32,
    compile_units: u32,
    manager_time_limit_s: f64,
) -> u64 {
    EvaluationTimeoutBudget {
        manager_time_limit_s,
        manager_wall_time_multiplier: sandbox.exec_wall_time_multiplier,
        manager_extra_time_s: sandbox.exec_extra_time_s,
        ..sandbox.timeout_budget(time_limit_ms, compile_units)
    }
    .timeout_ms()
}
