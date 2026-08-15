//! `chekov status` — running?, pid, model, revision, port, ctx, uptime,
//! wired-limit actual vs required, log tail path.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct StatusCmd {}

impl Command for StatusCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5b red")
    }
}
