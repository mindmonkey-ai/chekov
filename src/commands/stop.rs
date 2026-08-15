//! `chekov stop` — SIGTERM via pidfile, 20 s grace, SIGKILL escalation with a
//! warning; stale pidfiles detected and cleaned.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct StopCmd {}

impl Command for StopCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        use crate::core::server::{self, PidFile, StopOutcome};
        let pidfile = PidFile::new(ctx.config.pidfile());
        let Some(pid) = pidfile.read() else {
            return Err(ChekovError::ServerNotRunning);
        };
        if !server::process_alive(pid) {
            pidfile.remove()?;
            server::clear_run_state(&ctx.config)?;
            println!("stale pidfile (pid {pid} is dead) — cleaned up");
            return Ok(ExitCode::SUCCESS);
        }
        let outcome = server::stop_pid(pid, std::time::Duration::from_secs(20))?;
        pidfile.remove()?;
        server::clear_run_state(&ctx.config)?;
        match outcome {
            StopOutcome::Terminated => println!("stopped (pid {pid}, clean SIGTERM)"),
            StopOutcome::Killed => println!("stopped (pid {pid}, escalated to SIGKILL)"),
        }
        Ok(ExitCode::SUCCESS)
    }
}
