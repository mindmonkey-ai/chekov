//! `chekov rm <name>` — remove a model: confirmation required, refuses the
//! active or running model (§5 of the bootstrap prompt).

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct RmCmd {
    /// Registered model name.
    pub name: String,
    /// Skip the interactive confirmation.
    #[arg(long)]
    pub yes: bool,
}

impl Command for RmCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5a red")
    }
}
