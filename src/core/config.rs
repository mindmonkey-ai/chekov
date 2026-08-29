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
    pub engine: EngineSection,
}

/// Which llama.cpp the engine is built from.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EngineSection {
    /// A branch, tag, or commit to pin the engine to — anything `git fetch
    /// origin <ref>` accepts. Absent means whatever upstream HEAD is on the
    /// day `setup` or `update --engine` runs (a fast-forward pull).
    pub git_ref: Option<String>,
}

impl EngineSection {
    /// A ref git would read as an option, or one that would split into
    /// several arguments, cannot be a pin — refused at load, naming the key.
    fn validate(&self, path: &Path) -> Result<(), ChekovError> {
        let Some(git_ref) = self.git_ref.as_deref() else {
            return Ok(());
        };
        let reason = if git_ref.is_empty() {
            "[engine] git_ref is empty — remove the key to leave the engine unpinned"
        } else if git_ref.starts_with('-') {
            "[engine] git_ref starts with '-', which git would read as an option"
        } else if git_ref.chars().any(char::is_whitespace) {
            "[engine] git_ref contains whitespace — one branch, tag, or commit only"
        } else {
            return Ok(());
        };
        Err(ChekovError::ConfigInvalid {
            path: path.to_path_buf(),
            reason: reason.to_owned(),
        })
    }
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
    /// Sampling seed pinned onto every probe (greedy removes sampler
    /// nondeterminism; the seed pins what remains).
    pub seed: u32,
    /// Teardown waits until this percentage of the GPU budget is free again
    /// before the next candidate loads.
    pub release_pct: u32,
    pub release_max_polls: u32,
    pub release_interval_ms: u64,
    /// Tasks per `--codebase` run: two-thirds `in_file`, one-third
    /// `function_body`, sampled deterministically from HEAD.
    pub codebase_tasks: u32,
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
            seed: 42,
            release_pct: 80,
            release_max_polls: 60,
            release_interval_ms: 500,
            codebase_tasks: 24,
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
        let file: FileConfig = if path.exists() {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| ChekovError::io(format!("reading {}", path.display()), e))?;
            toml::from_str(&text).map_err(|e| ChekovError::ConfigInvalid {
                path: path.clone(),
                reason: e.to_string(),
            })?
        } else {
            FileConfig::default()
        };
        file.engine.validate(&path)?;
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

    /// Bench run directories (spec §7.5): `eval/<run_id>/`.
    #[must_use]
    pub fn eval_dir(&self) -> PathBuf {
        self.root.join("eval")
    }

    /// Rendered reports (`capability graph --svg`).
    #[must_use]
    pub fn reports_dir(&self) -> PathBuf {
        self.root.join("reports")
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

    use super::{BenchSection, Config, LimitsSection, resolve_root};
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
    fn codebase_tasks_defaults_to_24_and_overrides() {
        assert_eq!(BenchSection::default().codebase_tasks, 24);
        let root = scratch("cfg-codebase-tasks");
        std::fs::write(root.join("config.toml"), "[bench]\ncodebase_tasks = 12\n").expect("write");
        assert_eq!(
            Config::load(&root)
                .expect("valid")
                .file
                .bench
                .codebase_tasks,
            12
        );
    }

    #[test]
    fn bench_section_refuses_unknown_keys() {
        assert!(
            toml::from_str::<super::FileConfig>("[bench]\ntypo = 1\n").is_err(),
            "deny_unknown_fields (§C.7)"
        );
    }

    #[test]
    fn engine_section_parses_git_ref_and_defaults_to_unpinned() {
        assert_eq!(
            super::FileConfig::default().engine.git_ref,
            None,
            "no pin means today's behaviour: whatever upstream HEAD is"
        );
        let root = scratch("cfg-engine-ref");
        std::fs::write(root.join("config.toml"), "[engine]\ngit_ref = \"b7000\"\n").expect("write");
        let cfg = Config::load(&root).expect("a pinned engine parses");
        assert_eq!(cfg.file.engine.git_ref.as_deref(), Some("b7000"));
    }

    #[test]
    fn an_engine_ref_git_would_read_as_an_option_is_refused_at_load() {
        // `git fetch origin -x` would parse the ref as a flag; a ref with
        // whitespace would split into several. Neither can be a pin.
        for bad in ["-x", "--upload-pack=evil", "v1 v2", ""] {
            let root = scratch("cfg-engine-bad");
            std::fs::write(
                root.join("config.toml"),
                format!("[engine]\ngit_ref = {bad:?}\n"),
            )
            .expect("write");
            let msg = Config::load(&root).expect_err("must refuse").to_string();
            assert!(msg.contains("git_ref"), "names the key for {bad:?}: {msg}");
            assert!(msg.contains("config.toml"), "names the file: {msg}");
        }
    }

    #[test]
    fn engine_section_refuses_unknown_keys() {
        let root = scratch("cfg-engine-unknown");
        std::fs::write(root.join("config.toml"), "[engine]\nbranch = \"master\"\n").expect("write");
        assert!(Config::load(&root).is_err());
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
