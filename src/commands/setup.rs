//! `chekov setup` — clone/pull + cmake Metal build of llama.cpp, then verify
//! the GPU wired limit, printing (never executing) the sudo command (STOP-2).

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct SetupCmd {
    /// Print the steps without executing them.
    #[arg(long)]
    pub dry_run: bool,
}

impl Command for SetupCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        use crate::core::{checks, engine};
        let cfg = &ctx.config;
        engine::run_steps(&engine::setup_steps(&cfg.engine_dir()), self.dry_run)?;
        for dir in [cfg.models_dir(), cfg.logs_dir()] {
            std::fs::create_dir_all(&dir)
                .map_err(|e| ChekovError::io(format!("creating {}", dir.display()), e))?;
        }
        let required = cfg.file.limits.wired_limit_mb;
        let actual = checks::wired_limit_mb();
        if self.dry_run {
            println!("[dry-run] would verify iogpu.wired_limit_mb >= {required} (now: {actual:?})");
            return Ok(ExitCode::SUCCESS);
        }
        match actual {
            Some(actual) if actual >= required => {
                println!("wired limit OK ({actual} MB >= {required} MB) — setup complete");
                Ok(ExitCode::SUCCESS)
            }
            Some(actual) => Err(ChekovError::SetupIncomplete {
                remaining: format!(
                    "iogpu.wired_limit_mb is {actual}, need {required}; run: \
                     sudo sysctl iogpu.wired_limit_mb={required}"
                ),
            }),
            None => Err(ChekovError::SetupIncomplete {
                remaining: format!(
                    "could not read iogpu.wired_limit_mb; run: \
                     sudo sysctl iogpu.wired_limit_mb={required} and re-run `chekov setup`"
                ),
            }),
        }
    }
}
