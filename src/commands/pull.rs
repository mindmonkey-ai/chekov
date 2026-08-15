//! `chekov pull <spec>` (§4.2).
//!
//! Resolve revision, download quant-matching files, snapshot the license,
//! register with defaults-seeded flags. Idempotent; a new revision never
//! repoints (that is `update`'s job).

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct PullCmd {
    /// `org/repo[:QUANT][@rev]` or a huggingface.co URL.
    pub spec: String,
    /// Override the derived short name.
    #[arg(long)]
    pub name: Option<String>,
    /// Plan only: print what would be downloaded and registered.
    #[arg(long)]
    pub dry_run: bool,
    /// Also snapshot the base model's license from this URL.
    #[arg(long)]
    pub license_url: Option<String>,
}

impl Command for PullCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5b red")
    }
}
