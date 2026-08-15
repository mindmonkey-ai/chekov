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
    format!(
        "# Managed by `chekov integrate hermes` — regenerate rather than hand-edit.\n\
         provider: custom\n\
         base_url: {base}/v1\n\
         api_key: {key}\n\
         model: {alias}\n\
         context_length: {ctx}\n",
        base = cfg.base_url(),
        key = cfg.file.server.api_key,
        alias = eff.name,
        ctx = eff.ctx_size,
    )
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

/// STOP-3 gates: ~/.hermes must already exist, and replacing an actively
/// configured non-custom provider needs explicit confirmation.
fn hermes_config_path(content: &str, assume_yes: bool) -> Result<std::path::PathBuf, ChekovError> {
    let home = directories::UserDirs::new().map_or_else(
        || std::path::PathBuf::from("/"),
        |u| u.home_dir().to_path_buf(),
    );
    let hermes_dir = home.join(".hermes");
    if !hermes_dir.exists() {
        // STOP-3: never create another tool's config tree behind its back.
        return Err(ChekovError::HermesConfigUnsafe {
            reason: format!(
                "{} does not exist — is Hermes installed?",
                hermes_dir.display()
            ),
        });
    }
    let path = hermes_dir.join("config.yaml");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.contains("provider:") && !existing.contains("provider: custom") {
        // STOP-3: an actively configured non-custom provider is being replaced.
        println!(
            "existing {} has a non-custom provider; intended replacement:\n{content}",
            path.display()
        );
        super::confirm("replace the existing Hermes provider config", assume_yes)?;
    }
    Ok(path)
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
    let content = render_hermes_yaml(&ctx.config, &eff);
    let path = hermes_config_path(&content, assume_yes)?;
    if write_managed(&path, &content)? {
        println!(
            "hermes now targets {} as '{}'",
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
