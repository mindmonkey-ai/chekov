//! `chekov restart [name]` — stop (if running) then run in the background;
//! swaps models in one motion.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct RestartCmd {
    /// Model to (re)start (defaults to the active model).
    pub name: Option<String>,
}

/// Warn when a bare `restart` is about to swap models. `active` stays the
/// correct target — `use X` then `restart` is the documented swap — but
/// unloading 100+ GB and loading a different model must never be silent.
fn switch_note(running: Option<&str>, target: &str) -> Option<String> {
    let running = running.filter(|r| *r != target)?;
    Some(format!(
        "restart: '{running}' is running but '{target}' is the active model — \
         restarting '{target}'; run `chekov restart {running}` to keep the \
         running model instead"
    ))
}

impl Command for RestartCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        use crate::core::server;
        // Resolve the running model BEFORE the stop clears run-state, or the
        // comparison always sees None and the swap stays silent.
        if server::live_pid(&ctx.config).is_some() {
            let running = server::read_run_state(&ctx.config);
            if self.name.is_none()
                && let Ok(reg) = ctx.registry()
                && let Ok(target) = reg.active_name()
                && let Some(note) = switch_note(running.as_deref(), target)
            {
                println!("{note}");
            }
            super::stop::StopCmd {}.run(ctx)?;
        }
        super::run::RunCmd {
            name: self.name.clone(),
            foreground: false,
            daemon: false,
        }
        .run(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::switch_note;

    #[test]
    fn a_bare_restart_that_swaps_models_says_so() {
        let note = switch_note(Some("qwen3.8-27b"), "minimax-m2.7")
            .expect("swapping the loaded model must not be silent");
        assert!(
            note.contains("qwen3.8-27b"),
            "must name what is running: {note}"
        );
        assert!(
            note.contains("minimax-m2.7"),
            "must name the new target: {note}"
        );
        assert!(
            note.contains("chekov restart qwen3.8-27b"),
            "must name the command that keeps the running model: {note}"
        );
    }

    #[test]
    fn restarting_the_same_model_is_quiet() {
        assert_eq!(switch_note(Some("minimax-m2.7"), "minimax-m2.7"), None);
    }

    #[test]
    fn nothing_running_is_quiet() {
        assert_eq!(switch_note(None, "minimax-m2.7"), None);
    }
}
