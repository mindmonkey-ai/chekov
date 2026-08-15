//! `chekov update [--engine] [--model] [--all] [--dry-run]`.
//!
//! Engine rebuild with old→new commit report; model re-resolve with the
//! STOP-4 license-diff gate before an atomic registry repoint. Old revisions
//! are never deleted.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
// Four independent CLI switches, not a state machine — clap flags are the
// one place a bool-per-option struct is the correct shape.
#[allow(clippy::struct_excessive_bools)]
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

/// STOP-4 arming rule: gate whenever a previously snapshotted license text is
/// no longer byte-identical (including its disappearance). A repo that never
/// had a license snapshot has nothing to diff.
#[must_use]
pub fn license_gate_needed(old: Option<&str>, new: Option<&str>) -> bool {
    let _ = (old, new);
    todo!("cycle 5b red")
}

impl Command for UpdateCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5b red")
    }
}

#[cfg(test)]
mod tests {
    use super::license_gate_needed;

    #[test]
    fn gate_arms_only_on_real_change() {
        assert!(!license_gate_needed(None, None));
        assert!(!license_gate_needed(None, Some("MIT")));
        assert!(!license_gate_needed(Some("MIT"), Some("MIT")));
        assert!(license_gate_needed(Some("MIT"), Some("MIT + rider")));
        assert!(license_gate_needed(Some("MIT"), None));
    }
}
