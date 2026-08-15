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
