//! `chekov update [--engine] [--model] [--all] [--dry-run]` — engine rebuild
//! with old→new commit report; model re-resolve with the STOP-4 license-diff
//! gate before an atomic registry repoint. Old revisions are never deleted.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct UpdateCmd {
    /// Update the llama.cpp engine (git pull + rebuild).
    #[arg(long)]
    pub engine: bool,
    /// Re-resolve the active model's repo revision.
    #[arg(long)]
    pub model: bool,
    /// Both.
    #[arg(long)]
    pub all: bool,
    /// Preview without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}

impl Command for UpdateCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5b red")
    }
}
