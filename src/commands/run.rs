//! `chekov run [name] [--daemon]` — start llama-server after loud preflight:
//! shard present, port free, wired limit sufficient, nothing already running.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct RunCmd {
    /// Registered model name (defaults to the active model).
    pub name: Option<String>,
    /// Detach: pidfile logs/chekov.pid, output to logs/llama-server.log.
    #[arg(long)]
    pub daemon: bool,
}

/// All four refusal gates, checked before anything starts (§C.2 — never
/// degrade, never auto-shrink, never fall back).
fn preflight(ctx: &Ctx, eff: &crate::core::registry::Effective) -> Result<(), ChekovError> {
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
    let required = cfg.file.limits.wired_limit_mb;
    match checks::wired_limit_mb() {
        Some((actual, _is_default)) if actual < required => {
            return Err(ChekovError::WiredLimitLow {
                actual_mb: actual,
                required_mb: required,
            });
        }
        Some(_) => {}
        None => eprintln!("warning: could not read iogpu.wired_limit_mb — proceeding unverified"),
    }
    Ok(())
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
        if self.daemon {
            let pid = server::spawn_daemon(&ctx.config, &eff)?;
            server::write_run_state(&ctx.config, &name)?;
            println!(
                "started '{name}' (pid {pid}) — log: {}",
                ctx.config.server_log().display()
            );
            Ok(ExitCode::SUCCESS)
        } else {
            println!("starting '{name}' in the foreground (ctrl-c to stop)");
            server::run_foreground(&ctx.config, &eff)
        }
    }
}
