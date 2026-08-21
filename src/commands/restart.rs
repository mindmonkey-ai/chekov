//! `chekov restart [name]` — stop (if running) then run in the background;
//! swaps models in one motion.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct RestartCmd {
    /// Model to (re)start (defaults to the active model).
    pub name: Option<String>,
}

impl Command for RestartCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        if crate::core::server::live_pid(&ctx.config).is_some() {
            super::stop::StopCmd {}.run(ctx)?;
        }
        super::run::RunCmd {
            name: self.name.clone(),
            foreground: false,
            daemon: false,
        }
        .run(ctx)
    }
}
