//! `chekov integrate hermes|claude` — external integrations with `.bak-<UTC>`
//! backups, STOP-3 confirmation, and idempotent no-op second runs.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::core::config::Config;
use crate::core::registry::Effective;
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct IntegrateCmd {
    #[command(subcommand)]
    pub target: IntegrateTarget,
}

#[derive(Debug, clap::Subcommand)]
pub enum IntegrateTarget {
    /// Write ~/.hermes/config.yaml pointing Hermes at the local server.
    Hermes {
        /// Skip the STOP-3 confirmation when replacing an existing config.
        #[arg(long)]
        yes: bool,
    },
    /// Generate bin/cclocal (local-model Claude Code launcher).
    Claude,
}

/// The hermes config content. Pure so tests pin the contract.
#[must_use]
pub fn render_hermes_yaml(cfg: &Config, eff: &Effective) -> String {
    let _ = (cfg, eff);
    todo!("cycle 5b red")
}

/// The cclocal launcher script. Pure so tests pin the contract.
#[must_use]
pub fn render_cclocal() -> String {
    todo!("cycle 5b red")
}

impl Command for IntegrateCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5b red")
    }
}

#[cfg(test)]
mod tests {
    use super::{render_cclocal, render_hermes_yaml};
    use crate::core::config::Config;
    use crate::core::registry::{ModelEntry, Registry};

    #[test]
    fn hermes_yaml_points_at_local_v1_with_effective_ctx() {
        let root = std::env::temp_dir().join("chekov-test-integrate");
        let _ = std::fs::create_dir_all(&root);
        let cfg = Config::load(&root).expect("defaults");
        let mut reg = Registry::default();
        reg.models.insert(
            "minimax-m2.7".into(),
            ModelEntry {
                repo: "unsloth/MiniMax-M2.7-GGUF".into(),
                quant: "UD-Q5_K_XL".into(),
                revision: "abc".into(),
                path: "models/minimax-m2.7@abc".into(),
                first_shard: "x.gguf".into(),
                hermes_ok: true,
                ctx_size: None,
                extra_flags: vec![],
            },
        );
        let eff = reg.effective("minimax-m2.7").expect("registered");
        let yaml = render_hermes_yaml(&cfg, &eff);
        assert!(yaml.contains("provider: custom"), "{yaml}");
        assert!(yaml.contains("http://127.0.0.1:8080/v1"), "{yaml}");
        assert!(yaml.contains("model: minimax-m2.7"), "{yaml}");
        assert!(yaml.contains("context_length: 98304"), "{yaml}");
    }

    #[test]
    fn cclocal_evals_env_and_execs_claude() {
        let script = render_cclocal();
        assert!(script.starts_with("#!/bin/sh"), "{script}");
        assert!(script.contains(r#"eval "$(chekov env)""#), "{script}");
        assert!(script.contains(r#"exec claude "$@""#), "{script}");
    }
}
