//! The candidate lifecycle — launch, wait for readiness, tear down — shared
//! by `bench` and `tune` so neither reimplements the other's refusal gates.

use crate::commands::Ctx;
use crate::core::bench::runner::PropsInfo;
use crate::core::proxy::serve::Upstream;
use crate::core::registry::Effective;
use crate::error::ChekovError;

/// The context a bench needs beyond `Ctx`, resolved and guarded up front.
pub struct Candidate {
    pub eff: Effective,
    pub pid: i32,
}

/// Preflight, flag hygiene, then a Metal-aware spawn — the same refusal
/// gates as `chekov run`, never a back door around them.
pub fn launch(ctx: &Ctx, eff: &Effective) -> Result<i32, ChekovError> {
    use crate::core::bench::lifecycle;
    use crate::core::server;
    crate::commands::run::preflight(ctx, eff)?;
    let argv = server::launch_args(&ctx.config, eff);
    match lifecycle::server_help(&ctx.config.engine_dir()) {
        Some(help) => {
            if let Some(flag) = lifecycle::unknown_flags(&argv, &help).into_iter().next() {
                return Err(ChekovError::BenchFlagUnknown { flag });
            }
        }
        None => {
            eprintln!("chekov: could not capture llama-server --help — flag hygiene unchecked");
        }
    }
    let pid = server::spawn_daemon_with_env(&ctx.config, eff, &[lifecycle::METAL_RESIDENCY])?;
    server::write_run_state(&ctx.config, &eff.name)?;
    eprintln!("chekov: started '{}' (pid {pid})", eff.name);
    Ok(pid)
}

/// Stop what we started, then verify the budget actually came back before
/// the next candidate loads (spec §7.3.8).
pub fn teardown(ctx: &Ctx, pid: i32) -> Result<(), ChekovError> {
    use crate::core::bench::lifecycle;
    use crate::core::machine;
    use crate::core::server::{self, PidFile};
    let cfg = &ctx.config;
    server::stop_pid(pid, std::time::Duration::from_secs(20))?;
    PidFile::new(cfg.pidfile()).remove()?;
    server::clear_run_state(cfg)?;
    let Some(budget) = machine::live_gpu_budget(&cfg.engine_dir()) else {
        eprintln!("chekov: budget release UNVERIFIED — the engine probe is unavailable");
        return Ok(());
    };
    let bench_cfg = &cfg.file.bench;
    let policy = lifecycle::ReleasePolicy {
        total_mib: budget.value,
        release_pct: bench_cfg.release_pct,
        max_polls: bench_cfg.release_max_polls,
        interval: std::time::Duration::from_millis(bench_cfg.release_interval_ms),
    };
    let free =
        lifecycle::wait_budget_released(policy, &mut || machine::live_gpu_free(&cfg.engine_dir()))?;
    eprintln!("chekov: budget released ({free} MiB free)");
    Ok(())
}

/// Wait for `/health` (watching the pid) then assert the loaded `/props`
/// context; returns what the server actually loaded.
pub fn ensure_ready(
    ctx: &Ctx,
    upstream: &Upstream,
    candidate: &Candidate,
) -> Result<PropsInfo, ChekovError> {
    use crate::core::bench::runner;
    let ready = runner::ReadyTarget {
        base_url: upstream.base_url.clone(),
        pid: candidate.pid,
    };
    runner::wait_ready(ctx.http.as_ref(), &ready, (&ctx.config.file.bench).into())?;
    runner::assert_props_ctx(
        &|| crate::core::proxy::serve::get_bearer(upstream, "/props"),
        candidate.eff.ctx_size,
    )
}

#[cfg(test)]
mod tests {
    use super::Candidate;

    #[test]
    fn a_candidate_names_its_model_and_pid() {
        let candidate = Candidate {
            eff: crate::core::registry::Effective {
                name: "m".into(),
                ctx_size: 4096,
                flags: vec![],
                entry: crate::core::registry::ModelEntry {
                    repo: "o/r".into(),
                    quant: "Q8_0".into(),
                    revision: "abc123def4567890".into(),
                    path: "models/m@abc123def456".into(),
                    first_shard: "m.gguf".into(),
                    hermes_ok: false,
                    ctx_size: None,
                    extra_flags: vec![],
                    role: None,
                },
            },
            pid: 42,
        };
        assert_eq!((candidate.eff.name.as_str(), candidate.pid), ("m", 42));
    }
}
