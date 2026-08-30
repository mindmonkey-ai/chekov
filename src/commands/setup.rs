//! `chekov setup` — clone/pull + cmake Metal build of llama.cpp, then read the
//! GPU budget; a configured floor is verified against it, printing (never
//! executing) the sudo command (STOP-2).

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct SetupCmd {
    /// Print the steps without executing them.
    #[arg(long)]
    pub dry_run: bool,
}

impl Command for SetupCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        use crate::core::engine;
        let cfg = &ctx.config;
        let pin = cfg.file.engine.git_ref.as_deref();
        engine::run_steps(&engine::setup_steps(&cfg.engine_dir(), pin), self.dry_run)?;
        for dir in [cfg.models_dir(), cfg.logs_dir()] {
            std::fs::create_dir_all(&dir)
                .map_err(|e| ChekovError::io(format!("creating {}", dir.display()), e))?;
        }
        // Record what was actually built, so `status` can name the engine and a
        // broken upgrade has a commit to go back to.
        if !self.dry_run
            && let Some(commit) = engine::current_commit(&cfg.engine_dir())
        {
            engine::record_commit(&cfg.logs_dir(), &commit)?;
        }
        let floor = cfg.file.limits.wired_limit_mb;
        let budget = crate::core::machine::live_gpu_budget(&cfg.engine_dir());
        if self.dry_run {
            let against = floor.map_or_else(String::new, |f| format!(" >= {f} MB"));
            let now = budget.map_or_else(
                || "unreadable".to_owned(),
                |b| format!("{} MiB ({})", b.value, b.provenance.label()),
            );
            println!("[dry-run] would verify the GPU budget{against} (now: {now})");
            return Ok(ExitCode::SUCCESS);
        }
        println!("{}", setup_verdict(floor, budget)?);
        Ok(ExitCode::SUCCESS)
    }
}

/// What setup says about this machine's budget. Without a floor, a readable
/// budget is the whole verification — the model's footprint is `run`'s job;
/// with one, the floor is checked the way it always was.
fn setup_verdict(
    floor: Option<u64>,
    budget: Option<crate::core::machine::Probed<u64>>,
) -> Result<String, ChekovError> {
    match (floor, budget) {
        (None, Some(b)) => Ok(format!(
            "GPU budget {} MiB ({}) — setup complete",
            b.value,
            b.provenance.label()
        )),
        (Some(required), Some(b)) if b.value >= required => Ok(format!(
            "wired limit OK ({} MiB, {} >= {required} MB) — setup complete",
            b.value,
            b.provenance.label()
        )),
        (Some(required), Some(b)) => Err(ChekovError::SetupIncomplete {
            remaining: format!(
                "the GPU budget is {} MiB but the configured floor needs {required}; run: \
                 sudo sysctl iogpu.wired_limit_mb={required}",
                b.value
            ),
        }),
        (Some(required), None) => Err(ChekovError::SetupIncomplete {
            remaining: format!(
                "could not read the GPU budget; run: sudo sysctl \
                 iogpu.wired_limit_mb={required} and re-run `chekov setup`"
            ),
        }),
        (None, None) => Err(ChekovError::SetupIncomplete {
            remaining: "could not read the GPU budget (neither `llama-server --list-devices` \
                        nor sysctl answered) — check the engine build with `chekov update \
                        --engine`, then re-run `chekov setup`"
                .to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::setup_verdict;
    use crate::core::machine::{Probed, Provenance};
    use crate::error::ChekovError;

    fn budget(mib: u64, provenance: Provenance) -> Probed<u64> {
        Probed::new(mib, provenance)
    }

    #[test]
    fn without_a_floor_setup_completes_on_any_readable_budget() {
        let line = setup_verdict(None, Some(budget(24_576, Provenance::EngineReported)))
            .expect("complete");
        assert_eq!(
            line,
            "GPU budget 24576 MiB (engine-reported) — setup complete"
        );
    }

    #[test]
    fn a_configured_floor_is_verified_the_way_it_always_was() {
        let ok =
            setup_verdict(Some(150_000), Some(budget(196_608, Provenance::Measured))).expect("met");
        assert_eq!(
            ok,
            "wired limit OK (196608 MiB, measured >= 150000 MB) — setup complete"
        );
        let err = setup_verdict(
            Some(150_000),
            Some(budget(24_576, Provenance::EngineReported)),
        )
        .expect_err("below the floor");
        assert!(
            matches!(&err, ChekovError::SetupIncomplete { remaining } if remaining.contains("sudo sysctl iogpu.wired_limit_mb=150000")),
            "{err}"
        );
    }

    #[test]
    fn an_unreadable_budget_without_a_floor_is_incomplete_but_names_no_sysctl() {
        let err = setup_verdict(None, None).expect_err("nothing to verify against");
        let ChekovError::SetupIncomplete { remaining } = err else {
            panic!("{err}");
        };
        assert!(
            remaining.contains("could not read the GPU budget"),
            "{remaining}"
        );
        assert!(
            !remaining.contains("sudo"),
            "no floor means no number to demand: {remaining}"
        );
    }
}
