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
        let _ = ctx;
        todo!("cycle 5b red")
    }
}
