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

/// Withhold the value following `--api-key`, positionally.
///
/// `show` output is what people paste into bug reports, and `launch_args`
/// carries the server key verbatim. Matching on position rather than on the
/// key's text also covers a stray `--api-key` parked in `extra_flags`, and
/// cannot mangle a path that happens to contain the same string. `launch_args`
/// itself is untouched — it is what actually executes.
fn redact_api_key(args: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(args.len());
    let mut redact_next = false;
    for arg in args {
        if std::mem::replace(&mut redact_next, false) {
            out.push("<api_key from config.toml>".to_owned());
            continue;
        }
        redact_next = arg == "--api-key";
        out.push(arg.clone());
    }
    out
}

/// The human-readable show block. Pure so tests pin the invocation line.
#[must_use]
pub fn render_show(cfg: &Config, eff: &Effective) -> String {
    use crate::core::{engine, server};
    let invocation = format!(
        "{} {}",
        engine::server_binary(&cfg.engine_dir()).display(),
        redact_api_key(&server::launch_args(cfg, eff)).join(" ")
    );
    let provenance_path = cfg.root.join(&eff.entry.path).join("LICENSE.provenance");
    let provenance = std::fs::read_to_string(&provenance_path)
        .map_or_else(|_| "none recorded".to_owned(), |t| t.trim_end().to_owned());
    format!(
        "model:     {name}\nrepo:      {repo}\nquant:     {quant}\nrevision:  {rev}\n\
         hermes_ok: {hermes}\nctx_size:  {ctx}\n\ninvocation:\n  {invocation}\n\n\
         license provenance:\n  {provenance}\n",
        name = eff.name,
        repo = eff.entry.repo,
        quant = eff.entry.quant,
        rev = eff.entry.revision,
        hermes = eff.entry.hermes_ok,
        ctx = eff.ctx_size,
    )
}

impl Command for ShowCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let reg = ctx.registry()?;
        let name = match &self.name {
            Some(name) => name.as_str(),
            None => reg.active_name()?,
        };
        print!("{}", render_show(&ctx.config, &reg.effective(name)?));
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::render_show;
    use crate::core::config::Config;
    use crate::core::registry::{ModelEntry, Registry};

    #[test]
    fn the_server_api_key_is_not_printed_in_the_invocation() {
        let root = std::env::temp_dir().join("chekov-test-show-key");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch");
        std::fs::write(
            root.join("config.toml"),
            "[server]\napi_key = \"super-secret-token\"\n",
        )
        .expect("config");
        let cfg = Config::load(&root).expect("config");
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
                extra_flags: vec![],
                role: None,
            },
        );
        let eff = reg.effective("m").expect("registered");
        let out = render_show(&cfg, &eff);
        assert!(
            !out.contains("super-secret-token"),
            "`chekov show` output is what users paste into bug reports: {out}"
        );
        assert!(
            out.contains("--api-key"),
            "the flag must still be visible — only its value is withheld: {out}"
        );
    }

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
                role: None,
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
