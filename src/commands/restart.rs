//! `chekov restart [name]` — stop (if running) then run --daemon; swaps
//! models in one motion.

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
        let _ = ctx;
        todo!("cycle 5b red")
    }
}
