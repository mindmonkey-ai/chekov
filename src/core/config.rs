//! Global configuration (§16.11): every tunable lives here or in
//! `config.toml`, never inline in domain code. All structs deserialize with
//! `deny_unknown_fields` (§C.7).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::ChekovError;

/// On-disk `config.toml` shape. Missing file → all defaults.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FileConfig {
    pub server: ServerSection,
    pub limits: LimitsSection,
    pub doctor: DoctorSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerSection {
    pub host: String,
    pub port: u16,
    pub api_key: String,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8080,
            api_key: "chekov-local".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsSection {
    /// Minimum `iogpu.wired_limit_mb` required before `run` will start.
    pub wired_limit_mb: u64,
    /// Hermes needs at least this effective ctx when a model is `hermes_ok`.
    pub hermes_ctx_floor: u32,
}

impl Default for LimitsSection {
    fn default() -> Self {
        Self {
            wired_limit_mb: 200_000,
            hermes_ctx_floor: 65_536,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DoctorSection {
    /// Token budget for the NaN-canary generation.
    pub canary_max_tokens: u32,
    /// Consecutive identical tokens counted as degenerate.
    pub degenerate_run_len: usize,
    /// Replacement-char (U+FFFD) density treated as degenerate, in percent.
    pub replacement_char_max_pct: u8,
}

impl Default for DoctorSection {
    fn default() -> Self {
        Self {
            canary_max_tokens: 1_500,
            degenerate_run_len: 30,
            replacement_char_max_pct: 5,
        }
    }
}

/// Resolved runtime configuration: root directory + file settings.
#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub file: FileConfig,
}

impl Config {
    /// Load `<root>/config.toml` (defaults when absent, loud when invalid).
    pub fn load(root: &Path) -> Result<Self, ChekovError> {
        let _ = root;
        todo!("cycle 2 red")
    }

    #[must_use]
    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    #[must_use]
    pub fn registry_path(&self) -> PathBuf {
        self.root.join("models.toml")
    }

    #[must_use]
    pub fn pidfile(&self) -> PathBuf {
        self.logs_dir().join("chekov.pid")
    }

    #[must_use]
    pub fn server_log(&self) -> PathBuf {
        self.logs_dir().join("llama-server.log")
    }

    #[must_use]
    pub fn engine_dir(&self) -> PathBuf {
        self.root.join("llama.cpp")
    }

    /// `http://host:port` — the base every probe and integration derives from.
    #[must_use]
    pub fn base_url(&self) -> String {
        todo!("cycle 2 red")
    }
}

/// Root directory: `$CHEKOV_HOME` when set, else `~/personal_dev/chekov`.
#[must_use]
pub fn resolve_root(env_home: Option<&str>, user_home: &Path) -> PathBuf {
    let _ = (env_home, user_home);
    todo!("cycle 2 red")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Config, resolve_root};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chekov-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn defaults_when_file_missing() {
        let root = scratch("cfg-defaults");
        let cfg = Config::load(&root).expect("defaults");
        assert_eq!(cfg.file.server.port, 8080);
        assert_eq!(cfg.file.limits.hermes_ctx_floor, 65_536);
        assert_eq!(cfg.file.doctor.degenerate_run_len, 30);
    }

    #[test]
    fn parses_overrides() {
        let root = scratch("cfg-overrides");
        std::fs::write(root.join("config.toml"), "[server]\nport = 9090\n").expect("write");
        let cfg = Config::load(&root).expect("valid override");
        assert_eq!(cfg.file.server.port, 9090);
        assert_eq!(cfg.file.server.host, "127.0.0.1");
    }

    #[test]
    fn rejects_unknown_keys_loudly() {
        let root = scratch("cfg-unknown");
        std::fs::write(root.join("config.toml"), "[server]\nprot = 9090\n").expect("write");
        let msg = Config::load(&root).expect_err("must reject").to_string();
        assert!(msg.contains("config.toml"), "no path in: {msg}");
    }

    #[test]
    fn base_url_joins_host_and_port() {
        let root = scratch("cfg-baseurl");
        let cfg = Config::load(&root).expect("defaults");
        assert_eq!(cfg.base_url(), "http://127.0.0.1:8080");
    }

    #[test]
    fn root_prefers_env_over_default() {
        let home = Path::new("/Users/nobody");
        assert_eq!(resolve_root(Some("/x/y"), home), PathBuf::from("/x/y"));
        assert_eq!(
            resolve_root(None, home),
            PathBuf::from("/Users/nobody/personal_dev/chekov")
        );
    }
}
