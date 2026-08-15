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

/// The `model:` block chekov manages (active-model selection).
#[must_use]
pub fn render_model_block(cfg: &Config, eff: &Effective) -> String {
    format!(
        "model:\n\
         \x20\x20api_key: {key}\n\
         \x20\x20base_url: {base}/v1\n\
         \x20\x20context_length: {ctx}\n\
         \x20\x20default: {alias}\n\
         \x20\x20provider: chekov\n",
        base = cfg.base_url(),
        key = cfg.file.server.api_key,
        alias = eff.name,
        ctx = eff.ctx_size,
    )
}

/// The `chekov` entry under `providers:` (two-space indented child).
#[must_use]
pub fn render_provider_entry(cfg: &Config, eff: &Effective) -> String {
    format!(
        "\x20\x20chekov:\n\
         \x20\x20\x20\x20api: {base}/v1\n\
         \x20\x20\x20\x20api_key: {key}\n\
         \x20\x20\x20\x20default_model: {alias}\n\
         \x20\x20\x20\x20models:\n\
         \x20\x20\x20\x20\x20\x20- {alias}\n\
         \x20\x20\x20\x20name: chekov (llama.cpp local)\n",
        base = cfg.base_url(),
        key = cfg.file.server.api_key,
        alias = eff.name,
    )
}

/// Merge chekov's model + provider blocks into an existing Hermes config.
///
/// Every other line stays byte-identical. Wholesale replacement is forbidden:
/// a live Hermes config carries MCP servers, toolsets, and plugin state that
/// chekov must never clobber (STOP-3 spirit).
#[must_use]
pub fn merge_hermes_config(existing: &str, cfg: &Config, eff: &Effective) -> String {
    let model_block = render_model_block(cfg, eff);
    let mut text = if let Some(range) = top_block(existing, "model:") {
        let mut t = existing.to_owned();
        t.replace_range(range, &model_block);
        t
    } else {
        let mut t = model_block;
        t.push_str(existing);
        t
    };
    let entry = render_provider_entry(cfg, eff);
    if let Some(range) = top_block(&text, "providers:") {
        let updated = replace_child(&text[range.clone()], "  chekov:", &entry);
        text.replace_range(range, &updated);
    } else {
        text.push_str("providers:\n");
        text.push_str(&entry);
    }
    text
}

/// Byte range of a top-level YAML block: the `key` header line plus every
/// following line until the next top-level key.
fn top_block(text: &str, key: &str) -> Option<std::ops::Range<usize>> {
    let mut start = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let is_top = !line.starts_with([' ', '\t', '#', '\n']) && line.contains(':');
        if let Some(s) = start {
            if is_top {
                return Some(s..offset);
            }
        } else if is_top && line.trim_end() == key {
            start = Some(offset);
        }
        offset += line.len();
    }
    start.map(|s| s..text.len())
}

/// Replace (or insert after the header) one two-space-indented child entry
/// inside a top-level block.
fn replace_child(block: &str, child_header: &str, entry: &str) -> String {
    let mut start = None;
    let mut end = block.len();
    let mut offset = 0;
    for line in block.split_inclusive('\n') {
        let is_child_key =
            line.starts_with("  ") && !line.starts_with("   ") && line.trim_end().ends_with(':');
        if start.is_some() {
            if is_child_key {
                end = offset;
                break;
            }
        } else if line.trim_end() == child_header.trim_end() {
            start = Some(offset);
        }
        offset += line.len();
    }
    let mut out = block.to_owned();
    if let Some(s) = start {
        out.replace_range(s..end, entry);
    } else {
        let after_header = block.find('\n').map_or(block.len(), |i| i + 1);
        out.insert_str(after_header, entry);
    }
    out
}

/// The provider currently selected in the existing config's `model:` block.
#[must_use]
pub fn current_provider(existing: &str) -> Option<String> {
    let range = top_block(existing, "model:")?;
    existing[range]
        .lines()
        .find_map(|l| l.trim().strip_prefix("provider:"))
        .map(|v| v.trim().to_owned())
}

/// The cclocal launcher script. Pure so tests pin the contract.
#[must_use]
pub fn render_cclocal() -> String {
    "#!/bin/sh\n\
     # Managed by `chekov integrate claude` — Claude Code against the local server.\n\
     # Cloud Claude Code stays the default; this launcher affects only itself.\n\
     eval \"$(chekov env)\"\n\
     exec claude \"$@\"\n"
        .to_owned()
}

/// Back up `path` to `<path>.bak-<UTC>` before any overwrite.
fn backup(path: &std::path::Path) -> Result<Option<std::path::PathBuf>, ChekovError> {
    if !path.exists() {
        return Ok(None);
    }
    let stamp = crate::core::clock::utc_compact_now();
    let dest = path.with_file_name(format!(
        "{}.bak-{stamp}",
        path.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    ));
    std::fs::copy(path, &dest)
        .map_err(|e| ChekovError::io(format!("backing up {}", path.display()), e))?;
    Ok(Some(dest))
}

/// Write `content` to `path` idempotently: no-op when identical, backup-then-
/// write otherwise. Returns true when something changed.
fn write_managed(path: &std::path::Path, content: &str) -> Result<bool, ChekovError> {
    if std::fs::read_to_string(path).is_ok_and(|existing| existing == content) {
        println!("{} is already up to date — no-op", path.display());
        return Ok(false);
    }
    if let Some(bak) = backup(path)? {
        println!("backed up existing file to {}", bak.display());
    }
    std::fs::write(path, content)
        .map_err(|e| ChekovError::io(format!("writing {}", path.display()), e))?;
    Ok(true)
}

/// STOP-3 gate: ~/.hermes must already exist — chekov never creates another
/// tool's config tree behind its back.
fn hermes_config_file() -> Result<std::path::PathBuf, ChekovError> {
    let home = directories::UserDirs::new().map_or_else(
        || std::path::PathBuf::from("/"),
        |u| u.home_dir().to_path_buf(),
    );
    let hermes_dir = home.join(".hermes");
    if !hermes_dir.exists() {
        return Err(ChekovError::HermesConfigUnsafe {
            reason: format!(
                "{} does not exist — is Hermes installed?",
                hermes_dir.display()
            ),
        });
    }
    Ok(hermes_dir.join("config.yaml"))
}

fn integrate_hermes(ctx: &Ctx, assume_yes: bool) -> Result<ExitCode, ChekovError> {
    let reg = ctx.registry()?;
    let eff = reg.effective(reg.active_name()?)?;
    let floor = ctx.config.file.limits.hermes_ctx_floor;
    if eff.entry.hermes_ok && eff.ctx_size < floor {
        return Err(ChekovError::CtxBelowHermesFloor {
            name: eff.name.clone(),
            ctx: eff.ctx_size,
            floor,
        });
    }
    let path = hermes_config_file()?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let merged = merge_hermes_config(&existing, &ctx.config, &eff);
    if let Some(active) = current_provider(&existing).filter(|p| p != "chekov") {
        // STOP-3: an actively configured non-chekov provider is being switched.
        println!(
            "hermes currently uses provider '{active}'; chekov will repoint the model: \
             block (only) to:\n{}",
            render_model_block(&ctx.config, &eff)
        );
        super::confirm("switch the active Hermes provider to chekov", assume_yes)?;
    }
    if write_managed(&path, &merged)? {
        println!(
            "hermes now targets {} as '{}' (provider entry 'chekov'; all other \
             config sections untouched)",
            ctx.config.base_url(),
            eff.name
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn integrate_claude(ctx: &Ctx) -> Result<ExitCode, ChekovError> {
    use std::os::unix::fs::PermissionsExt;
    let bin_dir = ctx.config.root.join("bin");
    std::fs::create_dir_all(&bin_dir)
        .map_err(|e| ChekovError::io(format!("creating {}", bin_dir.display()), e))?;
    let path = bin_dir.join("cclocal");
    if write_managed(&path, &render_cclocal())? {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| ChekovError::io(format!("chmod {}", path.display()), e))?;
        println!(
            "wrote {} — global Claude settings untouched",
            path.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

impl Command for IntegrateCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        match &self.target {
            IntegrateTarget::Hermes { yes } => integrate_hermes(ctx, *yes),
            IntegrateTarget::Claude => integrate_claude(ctx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{current_provider, merge_hermes_config, render_cclocal};
    use crate::core::config::Config;
    use crate::core::registry::{ModelEntry, Registry};

    const EXISTING: &str = "model:\n\
        \x20\x20api_key: ollama\n\
        \x20\x20base_url: http://127.0.0.1:11434/v1\n\
        \x20\x20default: qwen3.6:35b\n\
        \x20\x20provider: ollama-launch\n\
        providers:\n\
        \x20\x20ollama-launch:\n\
        \x20\x20\x20\x20api: http://127.0.0.1:11434/v1\n\
        \x20\x20\x20\x20name: Ollama\n\
        toolsets:\n\
        \x20\x20- hermes-cli\n\
        agent:\n\
        \x20\x20max_turns: 150\n";

    fn fixture() -> (Config, crate::core::registry::Effective) {
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
        (cfg, eff)
    }

    #[test]
    fn merge_repoints_model_block_only() {
        let (cfg, eff) = fixture();
        let merged = merge_hermes_config(EXISTING, &cfg, &eff);
        assert!(merged.contains("provider: chekov"), "{merged}");
        assert!(
            merged.contains("base_url: http://127.0.0.1:8080/v1"),
            "{merged}"
        );
        assert!(merged.contains("default: minimax-m2.7"), "{merged}");
        assert!(merged.contains("context_length: 98304"), "{merged}");
        assert!(
            !merged.contains("api_key: ollama"),
            "old model block left: {merged}"
        );
    }

    #[test]
    fn merge_preserves_every_other_section() {
        let (cfg, eff) = fixture();
        let merged = merge_hermes_config(EXISTING, &cfg, &eff);
        for kept in [
            "toolsets:",
            "- hermes-cli",
            "max_turns: 150",
            "  ollama-launch:",
            "name: Ollama",
        ] {
            assert!(merged.contains(kept), "lost {kept:?}: {merged}");
        }
    }

    #[test]
    fn merge_inserts_chekov_provider_entry() {
        let (cfg, eff) = fixture();
        let merged = merge_hermes_config(EXISTING, &cfg, &eff);
        assert!(merged.contains("\n  chekov:\n"), "{merged}");
        assert!(
            merged.contains("    api: http://127.0.0.1:8080/v1"),
            "{merged}"
        );
        assert!(
            merged.contains("    default_model: minimax-m2.7"),
            "{merged}"
        );
    }

    #[test]
    fn merge_is_idempotent() {
        let (cfg, eff) = fixture();
        let once = merge_hermes_config(EXISTING, &cfg, &eff);
        let twice = merge_hermes_config(&once, &cfg, &eff);
        assert_eq!(once, twice);
    }

    #[test]
    fn merge_into_empty_builds_minimal_config() {
        let (cfg, eff) = fixture();
        let merged = merge_hermes_config("", &cfg, &eff);
        assert!(merged.starts_with("model:\n"), "{merged}");
        assert!(merged.contains("providers:\n  chekov:\n"), "{merged}");
    }

    #[test]
    fn current_provider_reads_model_block() {
        assert_eq!(current_provider(EXISTING).as_deref(), Some("ollama-launch"));
        assert_eq!(current_provider(""), None);
    }

    #[test]
    fn cclocal_evals_env_and_execs_claude() {
        let script = render_cclocal();
        assert!(script.starts_with("#!/bin/sh"), "{script}");
        assert!(script.contains(r#"exec claude "$@""#), "{script}");
    }

    /// Regression: `eval "$(chekov env)"` with chekov missing from PATH left
    /// the env empty and claude silently fell through to the CLOUD — the
    /// launcher must abort loudly instead (§C.2 never-degrade).
    #[test]
    fn cclocal_aborts_when_chekov_env_fails() {
        let script = render_cclocal();
        assert!(script.contains("exit 1"), "no loud failure path: {script}");
        assert!(
            script.contains("chekov env") && script.contains("|| {"),
            "env eval is unguarded: {script}"
        );
        let eval_pos = script.find("eval").expect("eval present");
        let exec_pos = script.find("exec claude").expect("exec present");
        assert!(eval_pos < exec_pos, "guard must precede exec: {script}");
    }
}
