//! `chekov stop` — SIGTERM via pidfile, 20 s grace, SIGKILL escalation with a
//! warning; stale pidfiles detected and cleaned.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct StopCmd {
    /// Exit 0 with "nothing to stop" when no server is running — for
    /// teardown scripts that must be idempotent. Opt-in: without it, stopping
    /// a stopped server is the loud failure it always was.
    #[arg(long)]
    pub if_running: bool,
}

/// What `stop` does when there is no pidfile at all.
fn absent_outcome(if_running: bool) -> Result<ExitCode, ChekovError> {
    if if_running {
        println!("nothing to stop — no chekov-managed server is running");
        Ok(ExitCode::SUCCESS)
    } else {
        Err(ChekovError::ServerNotRunning)
    }
}

impl Command for StopCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        use crate::core::server::{self, PidFile, StopOutcome};
        let pidfile = PidFile::new(ctx.config.pidfile());
        let Some(pid) = pidfile.read() else {
            return absent_outcome(self.if_running);
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

#[cfg(test)]
mod tests {
    use std::process::ExitCode;

    use super::absent_outcome;
    use crate::error::ChekovError;

    #[test]
    fn nothing_to_stop_is_loud_by_default_and_a_clean_zero_only_when_asked() {
        // A teardown script needs an idempotent stop; everyone else still
        // gets the loud refusal — a silent no-op by default would weaken the
        // loud-failure creed.
        assert!(matches!(
            absent_outcome(false),
            Err(ChekovError::ServerNotRunning)
        ));
        assert!(matches!(absent_outcome(true), Ok(code) if code == ExitCode::SUCCESS));
    }
}
