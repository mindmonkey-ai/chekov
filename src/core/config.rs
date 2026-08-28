//! Global configuration (§16.11): every tunable lives here or in
//! `config.toml`, never inline in domain code. All structs deserialize with
//! `deny_unknown_fields` (§C.7).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::ChekovError;

/// On-disk `config.toml` shape. Missing file → all defaults.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FileConfig {
    pub server: ServerSection,
    pub limits: LimitsSection,
    pub doctor: DoctorSection,
    pub bench: BenchSection,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
            wired_limit_mb: 187_000,
            hermes_ctx_floor: 65_536,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

/// `chekov capability bench` tunables (§6: knobs live here, not in code).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BenchSection {
    /// Prompt depths (approximate tokens) the sweep measures, ascending.
    pub depths: Vec<u32>,
    /// Probes per depth; `core::stats` drops the first as warmup.
    pub repetitions: u32,
    /// Decode length per probe — long enough to measure, short enough to end.
    pub max_tokens: u32,
    /// Median delta (percent) below which two runs are "no significant difference".
    pub significance_pct: u32,
    /// Readiness poll budget: attempts × interval. 600 × 500ms covers the
    /// ~2-minute load of a ~158 GiB model with headroom.
    pub ready_max_polls: u32,
    pub ready_interval_ms: u64,
}

impl Default for BenchSection {
    fn default() -> Self {
        Self {
            depths: vec![1024, 4096, 16384],
            repetitions: 5,
            max_tokens: 128,
            significance_pct: 5,
            ready_max_polls: 600,
            ready_interval_ms: 500,
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
        let path = root.join("config.toml");
        let file = if path.exists() {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| ChekovError::io(format!("reading {}", path.display()), e))?;
            toml::from_str(&text).map_err(|e| ChekovError::ConfigInvalid {
                path: path.clone(),
                reason: e.to_string(),
            })?
        } else {
            FileConfig::default()
        };
        Ok(Self {
            root: root.to_path_buf(),
            file,
        })
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

    /// Generated agent config dirs (`chekov launch`), one per agent slug.
    #[must_use]
    pub fn agent_dir(&self, slug: &str) -> PathBuf {
        self.root.join("agents").join(slug)
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

    /// Bench run records, one JSON file per run.
    #[must_use]
    pub fn bench_dir(&self) -> PathBuf {
        self.logs_dir().join("bench")
    }

    /// `http://host:port` — the base every probe and integration derives from.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.file.server.host, self.file.server.port)
    }
}

/// Root directory: `$CHEKOV_HOME` when set, else `~/.chekov`.
#[must_use]
pub fn resolve_root(env_home: Option<&str>, user_home: &Path) -> PathBuf {
    env_home.map_or_else(|| user_home.join(".chekov"), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Config, LimitsSection, resolve_root};
    use crate::core::checks::{WiredVerdict, effective_wired_mb, wired_verdict};

    /// The README's config block is the only place a user learns these values.
    /// Nothing previously stopped it drifting from the code — which is exactly
    /// how it came to document a `wired_limit_mb` the code did not use.
    #[test]
    fn the_readme_config_block_matches_the_shipped_defaults() {
        let readme = include_str!("../../README.md");
        let fence = readme
            .split("```toml")
            .skip(1)
            .filter_map(|rest| rest.split("```").next())
            .find(|f| f.contains("wired_limit_mb"))
            .expect("README documents the config.toml shape");
        let documented: super::FileConfig =
            toml::from_str(fence).expect("the documented config must parse as a real FileConfig");
        assert_eq!(
            documented,
            super::FileConfig::default(),
            "README's config block has drifted from the code defaults"
        );
    }

    #[test]
    fn the_shipped_default_is_satisfiable_on_a_stock_mac() {
        // A 256 GB Mac with `iogpu.wired_limit_mb` unset: macOS's own default
        // is 75% of RAM. A fresh `cargo install` must not refuse there.
        let (actual_mb, is_system_default) = effective_wired_mb(0, 274_877_906_944);
        assert!(is_system_default, "0 means the macOS default, not zero");
        assert_eq!(
            wired_verdict(LimitsSection::default().wired_limit_mb, actual_mb, 262_144),
            WiredVerdict::Satisfied,
            "the built-in requirement must be met by a stock machine at {actual_mb} MB"
        );
    }

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
    fn bench_section_defaults_and_overrides_parse() {
        let cfg: super::FileConfig = toml::from_str("").expect("empty config is all defaults");
        assert_eq!(cfg.bench.depths, vec![1024, 4096, 16384]);
        assert_eq!(cfg.bench.repetitions, 5);
        assert_eq!(cfg.bench.significance_pct, 5);
        let cfg: super::FileConfig =
            toml::from_str("[bench]\ndepths = [2048]\nrepetitions = 3\n").expect("overrides parse");
        assert_eq!(cfg.bench.depths, vec![2048]);
        assert_eq!(cfg.bench.repetitions, 3);
        assert_eq!(cfg.bench.max_tokens, 128, "unset keys keep their defaults");
    }

    #[test]
    fn bench_section_refuses_unknown_keys() {
        assert!(
            toml::from_str::<super::FileConfig>("[bench]\ntypo = 1\n").is_err(),
            "deny_unknown_fields (§C.7)"
        );
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
            PathBuf::from("/Users/nobody/.chekov")
        );
    }
}
