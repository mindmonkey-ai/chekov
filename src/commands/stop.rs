//! `chekov stop` — SIGTERM via pidfile, 20 s grace, SIGKILL escalation with a
//! warning; stale pidfiles detected and cleaned.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct StopCmd {}

impl Command for StopCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5b red")
    }
}
