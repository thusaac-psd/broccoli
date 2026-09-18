use broccoli_server_sdk::types::{EvaluationTimeoutBudget, ResourceLimits, seconds_from_ms};
use serde::{Deserialize, Deserializer, de};

fn default_num_processes() -> u32 {
    1
}

fn default_max_processes() -> u32 {
    64
}

fn default_fifo_buffer_size() -> u32 {
    8192
}

fn deserialize_nonzero_u32<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let v = u32::deserialize(deserializer)?;
    if v == 0 {
        return Err(de::Error::custom("num_processes must be >= 1"));
    }
    Ok(v)
}

/// A single manager source file entry (filename + content hash).
#[derive(Debug, Clone, Deserialize)]
pub struct ManagerSourceEntry {
    pub filename: String,
    pub hash: String,
}

/// Per-problem communication config (from `[config.communication]`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CommConfig {
    #[serde(
        deserialize_with = "deserialize_nonzero_u32",
        default = "default_num_processes"
    )]
    pub num_processes: u32,
    #[serde(default = "default_max_processes")]
    pub max_processes: u32,
    #[serde(default = "default_fifo_buffer_size")]
    pub fifo_buffer_size: u32,
    pub communication_mode: CommunicationMode,
    pub manager_language: String,
    /// Manager source files. Each entry maps a filename to a blob hash.
    /// The first entry is the primary source file used for language resolution.
    #[serde(default)]
    pub manager_sources: Vec<ManagerSourceEntry>,
    pub manager_time_limit_s: f64,
    pub manager_memory_limit_kb: u32,
}

impl Default for CommConfig {
    fn default() -> Self {
        Self {
            num_processes: 1,
            max_processes: 64,
            fifo_buffer_size: 8192,
            communication_mode: CommunicationMode::Redirect,
            manager_language: "cpp".to_string(),
            manager_sources: vec![],
            manager_time_limit_s: 30.0,
            manager_memory_limit_kb: 524_288,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_timeout_accounts_for_compile_run_and_manager_budget() {
        let config = SandboxConfig {
            compile_time_limit_s: 120.0,
            compile_wall_time_multiplier: 3.0,
            exec_wall_time_multiplier: 5.0,
            ..SandboxConfig::default()
        };

        assert!(config.result_timeout_ms_for(300_000, 3, 120.0) > config.result_timeout_ms);
    }

    #[test]
    fn result_timeout_uses_configured_value_as_floor() {
        let config = SandboxConfig {
            result_timeout_ms: 1_500_000,
            ..SandboxConfig::default()
        };

        assert_eq!(config.result_timeout_ms_for(1000, 1, 30.0), 1_500_000);
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommunicationMode {
    #[default]
    Redirect,
    FifoArgs,
}

/// Sandbox resource limits (from `[config.sandbox]`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    pub compile_time_limit_s: f64,
    pub compile_extra_time_s: f64,
    pub compile_wall_time_multiplier: f64,
    pub compile_memory_limit_kb: u32,
    pub compile_process_limit: u32,
    pub compile_open_files_limit: u32,
    pub compile_file_size_limit_kb: u32,
    pub exec_extra_time_s: f64,
    pub exec_wall_time_multiplier: f64,
    pub exec_process_limit: u32,
    pub exec_open_files_limit: u32,
    pub exec_file_size_limit_kb: u32,
    pub result_timeout_ms: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            compile_time_limit_s: 30.0,
            compile_extra_time_s: 0.0,
            compile_wall_time_multiplier: 2.0,
            compile_memory_limit_kb: 524_288,
            compile_process_limit: 32,
            compile_open_files_limit: 256,
            compile_file_size_limit_kb: 524_288,
            exec_extra_time_s: 0.0,
            exec_wall_time_multiplier: 5.0,
            exec_process_limit: 1,
            exec_open_files_limit: 64,
            exec_file_size_limit_kb: 65_536,
            result_timeout_ms: EvaluationTimeoutBudget::default_for_time_limit_ms(0)
                .minimum_timeout_ms,
        }
    }
}

impl SandboxConfig {
    pub fn compile_limits(&self) -> ResourceLimits {
        ResourceLimits {
            time_limit: Some(self.compile_time_limit_s),
            wall_time_limit: Some(self.compile_time_limit_s * self.compile_wall_time_multiplier),
            extra_time: if self.compile_extra_time_s > 0.0 {
                Some(self.compile_extra_time_s)
            } else {
                None
            },
            memory_limit: Some(self.compile_memory_limit_kb),
            process_limit: Some(self.compile_process_limit),
            open_files_limit: Some(self.compile_open_files_limit),
            file_size_limit: Some(self.compile_file_size_limit_kb),
            ..Default::default()
        }
    }

    pub fn exec_limits(&self, time_limit_s: f64, memory_limit_kb: u32) -> ResourceLimits {
        ResourceLimits {
            time_limit: Some(time_limit_s),
            wall_time_limit: Some(time_limit_s * self.exec_wall_time_multiplier),
            extra_time: if self.exec_extra_time_s > 0.0 {
                Some(self.exec_extra_time_s)
            } else {
                None
            },
            memory_limit: Some(memory_limit_kb),
            process_limit: Some(self.exec_process_limit),
            open_files_limit: Some(self.exec_open_files_limit),
            file_size_limit: Some(self.exec_file_size_limit_kb),
            ..Default::default()
        }
    }

    pub fn manager_limits(
        &self,
        time_limit_s: f64,
        memory_limit_kb: u32,
        num_processes: u32,
    ) -> ResourceLimits {
        // Manager gets generous wall-time (many FIFO I/O waits) and single-process.
        // Uses exec open_files_limit (not compile) - the manager is a runtime process
        // that opens 2 FIFOs per contestant process plus stdin/stdout/stderr.
        //
        // The fd limit MUST scale with num_processes: the manager wires 2*N FIFOs
        // (m_to_c / c_to_m) plus a handful of standard/input fds, so a fixed floor
        // (64) exhausts once num_processes exceeds ~30 (max_processes defaults to
        // 64), and open() returns EMFILE -> the manager errors -> every submission
        // on that problem is a SystemError.
        let fd_floor = 2 * num_processes + 8;
        ResourceLimits {
            time_limit: Some(time_limit_s),
            wall_time_limit: Some(time_limit_s * self.exec_wall_time_multiplier),
            extra_time: if self.exec_extra_time_s > 0.0 {
                Some(self.exec_extra_time_s)
            } else {
                None
            },
            memory_limit: Some(memory_limit_kb),
            process_limit: Some(1),
            open_files_limit: Some(self.exec_open_files_limit.max(fd_floor).max(64)),
            file_size_limit: Some(self.exec_file_size_limit_kb),
            ..Default::default()
        }
    }

    pub fn result_timeout_ms_for(
        &self,
        time_limit_ms: i32,
        compile_units: u32,
        manager_time_limit_s: f64,
    ) -> u64 {
        EvaluationTimeoutBudget {
            compile_units,
            compile_time_limit_s: self.compile_time_limit_s,
            compile_wall_time_multiplier: self.compile_wall_time_multiplier,
            compile_extra_time_s: self.compile_extra_time_s,
            exec_time_limit_s: seconds_from_ms(time_limit_ms),
            exec_wall_time_multiplier: self.exec_wall_time_multiplier,
            exec_extra_time_s: self.exec_extra_time_s,
            manager_time_limit_s,
            manager_wall_time_multiplier: self.exec_wall_time_multiplier,
            manager_extra_time_s: self.exec_extra_time_s,
            minimum_timeout_ms: self.result_timeout_ms.max(
                EvaluationTimeoutBudget::default_for_time_limit_ms(time_limit_ms)
                    .minimum_timeout_ms,
            ),
            maximum_timeout_ms: self.result_timeout_ms.max(
                EvaluationTimeoutBudget::default_for_time_limit_ms(time_limit_ms)
                    .maximum_timeout_ms,
            ),
            ..EvaluationTimeoutBudget::default_for_time_limit_ms(time_limit_ms)
        }
        .timeout_ms()
    }
}
