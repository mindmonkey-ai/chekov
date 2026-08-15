//! `chekov doctor` — five independent checks, summary table, non-zero exit on
//! any failure. Skipped checks are reported as skipped, never as passed.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::core::config::Config;
use crate::core::hub::HttpClient;
use crate::core::registry::Effective;
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct DoctorCmd {}

/// One check's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Fail(String),
    Skipped(String),
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub name: &'static str,
    pub status: CheckStatus,
}

/// Run all five checks against the server (via the HTTP seam — tests inject
/// canned responses).
pub fn run_checks(http: &dyn HttpClient, cfg: &Config, eff: &Effective) -> Vec<CheckResult> {
    let _ = (http, cfg, eff);
    todo!("cycle 5b red")
}

impl Command for DoctorCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5b red")
    }
}
