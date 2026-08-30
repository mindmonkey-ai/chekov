//! `chekov setup` — clone/pull + cmake Metal build of llama.cpp, then verify
//! the GPU wired limit, printing (never executing) the sudo command (STOP-2).

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
        use crate::core::{checks, engine};
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
        let required = cfg.file.limits.wired_limit_mb;
        let actual = checks::wired_limit_mb();
        if self.dry_run {
            println!("[dry-run] would verify iogpu.wired_limit_mb >= {required} (now: {actual:?})");
            return Ok(ExitCode::SUCCESS);
        }
        match actual {
            Some((actual, is_default)) if actual >= required => {
                let origin = if is_default { ", system default" } else { "" };
                println!("wired limit OK ({actual} MB{origin} >= {required} MB) — setup complete");
                Ok(ExitCode::SUCCESS)
            }
            Some((actual, _)) => Err(ChekovError::SetupIncomplete {
                remaining: format!(
                    "iogpu.wired_limit_mb is {actual}, need {required}; run: \
                     sudo sysctl iogpu.wired_limit_mb={required}"
                ),
            }),
            None => Err(ChekovError::SetupIncomplete {
                remaining: format!(
                    "could not read iogpu.wired_limit_mb; run: \
                     sudo sysctl iogpu.wired_limit_mb={required} and re-run `chekov setup`"
                ),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::setup_verdict;
    use crate::core::machine::{Probed, Provenance};
    use crate::error::ChekovError;

    fn budget(mib: u64, provenance: Provenance) -> Option<Probed<u64>> {
        Some(Probed::new(mib, provenance))
    }

    #[test]
    fn without_a_floor_setup_completes_on_any_readable_budget() {
        let line =
            setup_verdict(None, budget(24_576, Provenance::EngineReported)).expect("complete");
        assert_eq!(
            line,
            "GPU budget 24576 MiB (engine-reported) — setup complete"
        );
    }

    #[test]
    fn a_configured_floor_is_verified_the_way_it_always_was() {
        let ok = setup_verdict(Some(150_000), budget(196_608, Provenance::Measured)).expect("met");
        assert_eq!(
            ok,
            "wired limit OK (196608 MiB, measured >= 150000 MB) — setup complete"
        );
        let err = setup_verdict(Some(150_000), budget(24_576, Provenance::EngineReported))
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
