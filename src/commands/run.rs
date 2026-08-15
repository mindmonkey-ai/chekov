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

impl Command for RunCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5b red")
    }
}
