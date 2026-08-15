//! `chekov show <name>` — the fully resolved server invocation plus license
//! provenance: no mystery about what will run (§4.3).

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::core::config::Config;
use crate::core::registry::Effective;
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct ShowCmd {
    /// Registered model name (defaults to the active model).
    pub name: Option<String>,
}

/// The human-readable show block. Pure so tests pin the invocation line.
#[must_use]
pub fn render_show(cfg: &Config, eff: &Effective) -> String {
    let _ = (cfg, eff);
    todo!("cycle 5a red")
}

impl Command for ShowCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5a red")
    }
}

#[cfg(test)]
mod tests {
    use super::render_show;
    use crate::core::config::Config;
    use crate::core::registry::{ModelEntry, Registry};

    #[test]
    fn show_prints_resolved_invocation_and_provenance_hint() {
        let root = std::env::temp_dir().join("chekov-test-show");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch");
        let cfg = Config::load(&root).expect("defaults");
        let mut reg = Registry::default();
        reg.models.insert(
            "m".into(),
            ModelEntry {
                repo: "org/repo".into(),
                quant: "Q8_0".into(),
                revision: "abc".into(),
                path: "models/m@abc".into(),
                first_shard: "m.gguf".into(),
                hermes_ok: true,
                ctx_size: None,
                extra_flags: vec!["--temp".into(), "1.0".into()],
            },
        );
        let eff = reg.effective("m").expect("registered");
        let text = render_show(&cfg, &eff);
        assert!(text.contains("llama-server"), "no binary: {text}");
        assert!(text.contains("--ctx-size 98304"), "no ctx: {text}");
        assert!(text.contains("--temp 1.0"), "extras missing: {text}");
        assert!(text.contains("org/repo"), "repo missing: {text}");
    }
}
