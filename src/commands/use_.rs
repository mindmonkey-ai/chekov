//! `chekov use <name>` — set the active model. Never auto-restarts a running
//! server; prints the restart hint instead (§5 of the bootstrap prompt).

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct UseCmd {
    /// Registered model name (see `chekov list`).
    pub name: String,
}

impl Command for UseCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5a red")
    }
}
