//! `print-client.toml`. One station drives many printers across many servers.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn default_poll() -> u64 {
    3
}
fn default_max_pages() -> u32 {
    10
}
fn default_paper() -> String {
    "A4".to_string()
}
fn default_font_size() -> f32 {
    9.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerCfg {
    pub url: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrinterCfg {
    /// Matched against a job's `target_printer`.
    pub name: String,
    /// CUPS or Windows queue name. Empty means the system default.
    #[serde(default)]
    pub os_id: Option<String>,
    /// `lp -d {printer} {file}`, or `folder:/path` to write a PDF instead.
    #[serde(default)]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Label printed on this station's output. The server knows the station by
    /// the name configured next to its token instead.
    pub station: String,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default = "default_poll")]
    pub poll_interval_secs: u64,
    #[serde(default = "default_max_pages")]
    pub max_pages: u32,
    #[serde(default = "default_paper")]
    pub paper: String,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    #[serde(default)]
    pub banner: String,
    #[serde(rename = "server", default)]
    pub servers: Vec<ServerCfg>,
    #[serde(rename = "printer", default)]
    pub printers: Vec<PrinterCfg>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            station: "station-1".to_string(),
            location: None,
            poll_interval_secs: default_poll(),
            max_pages: default_max_pages(),
            paper: default_paper(),
            font_size: default_font_size(),
            banner: String::new(),
            servers: Vec::new(),
            printers: Vec::new(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serializing config")?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        std::fs::write(path, text).with_context(|| format!("writing config {}", path.display()))?;
        Ok(())
    }

    pub fn printer_names(&self) -> Vec<String> {
        self.printers.iter().map(|p| p.name.clone()).collect()
    }
}

/// `PRINT_CLIENT_CONFIG` overrides the default of `./print-client.toml`.
pub fn default_config_path() -> PathBuf {
    if let Ok(env) = std::env::var("PRINT_CLIENT_CONFIG") {
        return PathBuf::from(env);
    }
    PathBuf::from("print-client.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let cfg = Config {
            station: "room-a".into(),
            location: Some("Room A".into()),
            poll_interval_secs: 5,
            max_pages: 8,
            paper: "Letter".into(),
            font_size: 10.0,
            banner: "Regionals".into(),
            servers: vec![ServerCfg {
                url: "http://judge.local:3000".into(),
                token: "tok".into(),
            }],
            printers: vec![PrinterCfg {
                name: "main".into(),
                os_id: Some("HP_1".into()),
                command: None,
            }],
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.station, "room-a");
        assert_eq!(back.servers.len(), 1);
        assert_eq!(back.printers[0].name, "main");
        assert_eq!(back.poll_interval_secs, 5);
    }

    #[test]
    fn applies_defaults_for_missing_fields() {
        let text = r#"
            station = "s"
            [[server]]
            url = "http://x"
            token = "t"
            [[printer]]
            name = "p"
        "#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert_eq!(cfg.poll_interval_secs, 3);
        assert_eq!(cfg.max_pages, 10);
        assert_eq!(cfg.paper, "A4");
        assert_eq!(cfg.font_size, 9.0);
        assert!(cfg.location.is_none());
    }
}
