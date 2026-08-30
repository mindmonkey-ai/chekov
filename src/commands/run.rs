//! `chekov run [name] [--foreground]` — start llama-server after loud preflight:
//! shard present, port free, the model within the GPU budget (or the
//! configured floor met), nothing already running.
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
    wired_gate(cfg, eff)
}

/// The memory gate. With a configured floor, the budget is judged against
/// it; without one — the default — the model itself is the requirement, so
/// its footprint is judged against the budget instead.
fn wired_gate(
    cfg: &crate::core::config::Config,
    eff: &crate::core::registry::Effective,
) -> Result<(), ChekovError> {
    // Same resolver `chekov capability` prints, so the gate and the report can
    // never disagree about what this machine can hold.
    let Some(budget) = crate::core::machine::live_gpu_budget(&cfg.engine_dir()) else {
        eprintln!("warning: could not determine the GPU budget — proceeding unverified");
        return Ok(());
    };
    cfg.file.limits.wired_limit_mb.map_or_else(
        || footprint_gate(cfg, eff, budget.value),
        |required_mb| floor_gate(cfg, required_mb, budget.value),
    )
}

/// The model's predicted footprint against the budget: over is a refusal
/// naming the levers that exist, tight is said out loud, and an unreadable
/// header proceeds with the check named as not done — never a silent pass.
fn footprint_gate(
    cfg: &crate::core::config::Config,
    eff: &crate::core::registry::Effective,
    budget_mib: u64,
) -> Result<(), ChekovError> {
    use crate::core::footprint::{Decision, decide, predicted_total};
    match decide(predicted_total(cfg, eff), budget_mib) {
        Decision::Proceed => Ok(()),
        Decision::Tight { pct } => {
            eprintln!(
                "tight: '{}' is {pct}% of the {budget_mib} MiB GPU budget",
                eff.name
            );
            Ok(())
        }
        Decision::Unverified => {
            eprintln!(
                "warning: could not predict the footprint of '{}' (weights or GGUF header \
                 unreadable) — proceeding unverified",
                eff.name
            );
            Ok(())
        }
        Decision::Exceeds { need_mib } => Err(ChekovError::ModelExceedsBudget {
            name: eff.name.clone(),
            need_mib,
            budget_mib,
            ctx: eff.ctx_size,
        }),
    }
}

/// The configured floor's refusal, split on whether the machine could ever
/// satisfy it. A requirement above physical RAM has no sysctl remedy, so
/// pointing the user at one would be a lie (§C.2 — every refusal names a real
/// remediation).
fn floor_gate(
    cfg: &crate::core::config::Config,
    required_mb: u64,
    actual_mb: u64,
) -> Result<(), ChekovError> {
    use crate::core::checks::{WiredVerdict, physical_ram_mb, wired_verdict};
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
