//! `chekov run [name] [--foreground]` — start llama-server after loud preflight:
//! shard present, port free, wired limit sufficient, nothing already running.
//!
//! Backgrounds by default (pidfile logs/chekov.pid, output to
//! logs/llama-server.log). `--foreground` blocks the terminal instead; the
//! legacy `--daemon` flag is a hidden no-op kept so old scripts still parse.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct RunCmd {
    /// Registered model name (defaults to the active model).
    pub name: Option<String>,
    /// Block the terminal instead of detaching (ctrl-c to stop).
    #[arg(long)]
    pub foreground: bool,
    /// Deprecated: backgrounding is now the default. Accepted as a no-op.
    #[arg(long, hide = true)]
    pub daemon: bool,
}

/// All four refusal gates, checked before anything starts (§C.2 — never
/// degrade, never auto-shrink, never fall back).
pub(crate) fn preflight(
    ctx: &Ctx,
    eff: &crate::core::registry::Effective,
) -> Result<(), ChekovError> {
    use crate::core::{checks, engine, server};
    let cfg = &ctx.config;
    if let Some(pid) = server::live_pid(cfg) {
        return Err(ChekovError::ServerAlreadyRunning { pid });
    }
    let binary = engine::server_binary(&cfg.engine_dir());
    if !binary.exists() {
        return Err(ChekovError::SetupIncomplete {
            remaining: format!("{} is missing — run `chekov setup`", binary.display()),
        });
    }
    let shard = server::shard_path(cfg, eff);
    if !shard.exists() {
        return Err(ChekovError::MissingShard {
            name: eff.name.clone(),
            path: shard,
        });
    }
    if checks::port_in_use(&cfg.file.server.host, cfg.file.server.port) {
        return Err(ChekovError::PortOccupied {
            port: cfg.file.server.port,
        });
    }
    wired_gate(cfg, cfg.file.limits.wired_limit_mb)
}

/// The wired-limit refusal, split on whether the machine could ever satisfy it.
/// A requirement above physical RAM has no sysctl remedy, so pointing the user
/// at one would be a lie (§C.2 — every refusal names a real remediation).
fn wired_gate(cfg: &crate::core::config::Config, required_mb: u64) -> Result<(), ChekovError> {
    use crate::core::checks::{WiredVerdict, physical_ram_mb, wired_verdict};
    let Some((actual_mb, _is_default)) = crate::core::checks::wired_limit_mb() else {
        eprintln!("warning: could not read iogpu.wired_limit_mb — proceeding unverified");
        return Ok(());
    };
    // Unreadable RAM must never upgrade a refusal to "unreachable" — stay loud
    // but stay accurate.
    let ram_mb = physical_ram_mb().unwrap_or(u64::MAX);
    match wired_verdict(required_mb, actual_mb, ram_mb) {
        WiredVerdict::Satisfied => Ok(()),
        WiredVerdict::Low => Err(ChekovError::WiredLimitLow {
            actual_mb,
            required_mb,
        }),
        WiredVerdict::Unreachable => Err(ChekovError::WiredLimitUnreachable {
            required_mb,
            ram_mb,
            config_path: cfg.root.join("config.toml"),
        }),
    }
}

impl Command for RunCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        use crate::core::server;
        let reg = ctx.registry()?;
        let name = match &self.name {
            Some(name) => name.clone(),
            None => reg.active_name()?.to_owned(),
        };
        let eff = reg.effective(&name)?;
        preflight(ctx, &eff)?;
        if self.foreground {
            println!("starting '{name}' in the foreground (ctrl-c to stop)");
            server::run_foreground(&ctx.config, &eff)
        } else {
            let pid = server::spawn_daemon(&ctx.config, &eff)?;
            server::write_run_state(&ctx.config, &name)?;
            println!(
                "started '{name}' (pid {pid}) — log: {}",
                ctx.config.server_log().display()
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}
