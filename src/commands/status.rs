//! `chekov status` — running?, pid, model, revision, port, ctx, uptime,
//! wired-limit actual vs required, log tail path.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct StatusCmd {}

/// `4262` seconds → `"1h 11m"`, `42` → `"42s"`.
#[must_use]
pub fn human_duration(secs: u64) -> String {
    let _ = secs;
    todo!("cycle 5b red")
}

impl Command for StatusCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5b red")
    }
}

#[cfg(test)]
mod tests {
    use super::human_duration;

    #[test]
    fn durations_scale_units() {
        assert_eq!(human_duration(42), "42s");
        assert_eq!(human_duration(300), "5m 00s");
        assert_eq!(human_duration(4262), "1h 11m");
        assert_eq!(human_duration(90_000), "1d 1h");
    }
}
