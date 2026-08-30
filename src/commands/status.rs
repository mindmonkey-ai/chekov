//! `chekov status` — running?, pid, model, revision, port, ctx, uptime,
//! the GPU budget and what `run` checks against it, log tail path.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct StatusCmd {}

/// `4262` seconds → `"1h 11m"`, `42` → `"42s"`.
#[must_use]
pub fn human_duration(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m {:02}s", s / 60, s % 60),
        s if s < 86_400 => format!("{}h {}m", s / 3_600, s % 3_600 / 60),
        s => format!("{}d {}h", s / 86_400, s % 86_400 / 3_600),
    }
}

fn uptime(ctx: &Ctx) -> String {
    let pidfile = ctx.config.pidfile();
    std::fs::metadata(pidfile)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map_or_else(|| "unknown".to_owned(), |d| human_duration(d.as_secs()))
}

/// (revision-12, effective ctx) for a model name, dashes when unknown.
fn model_facts(reg: &crate::core::registry::Registry, model: &str) -> (String, String) {
    let revision = reg
        .models
        .get(model)
        .map_or_else(|| "-".to_owned(), |e| e.revision.chars().take(12).collect());
    let ctx_size = reg
        .effective(model)
        .map_or_else(|_| "-".to_owned(), |e| e.ctx_size.to_string());
    (revision, ctx_size)
}

/// The GPU budget (resolved through the same function `chekov capability`
/// prints, so the gate and the report can never disagree) and what `run` will
/// judge against it: a configured floor, or the model's own footprint.
fn wired_cell(budget: Option<crate::core::machine::Probed<u64>>, floor: Option<u64>) -> String {
    let need = floor.map_or_else(
        || "no floor configured; run checks the model's footprint".to_owned(),
        |required| format!("need {required} MB"),
    );
    budget.map_or_else(
        || format!("unreadable ({need})"),
        |b| format!("{} MiB ({}) ({need})", b.value, b.provenance.label()),
    )
}

/// The engine commit chekov last built. Never guessed: an unrecorded engine
/// says so and names the command that records one.
fn engine_row(ctx: &Ctx) -> String {
    crate::core::engine::recorded_commit(&ctx.config.logs_dir())
        .unwrap_or_else(|| "unrecorded — run `chekov setup` or `chekov update --engine`".to_owned())
}

fn status_rows(ctx: &Ctx) -> Result<Vec<Vec<String>>, ChekovError> {
    use crate::core::server;
    let reg = ctx.registry()?;
    let pid = server::live_pid(&ctx.config);
    let model = server::read_run_state(&ctx.config)
        .or_else(|| reg.active.clone())
        .unwrap_or_else(|| "none".to_owned());
    let (revision, ctx_size) = model_facts(&reg, &model);
    let wired = wired_cell(
        crate::core::machine::live_gpu_budget(&ctx.config.engine_dir()),
        ctx.config.file.limits.wired_limit_mb,
    );
    Ok(vec![
        vec![
            "running".into(),
            pid.map_or_else(|| "no".into(), |p| format!("yes (pid {p})")),
        ],
        vec!["model".into(), model],
        vec!["revision".into(), revision],
        vec!["port".into(), ctx.config.file.server.port.to_string()],
        vec!["ctx".into(), ctx_size],
        vec![
            "uptime".into(),
            if pid.is_some() {
                uptime(ctx)
            } else {
                "-".into()
            },
        ],
        vec!["wired limit".into(), wired],
        vec!["engine".into(), engine_row(ctx)],
        vec![
            "log tail".into(),
            ctx.config.server_log().display().to_string(),
        ],
    ])
}

impl Command for StatusCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let rows = status_rows(ctx)?;
        println!("{}", super::render_table(&["FIELD", "VALUE"], &rows));
        Ok(ExitCode::SUCCESS)
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

    #[test]
    fn the_wired_row_says_what_run_will_check_against() {
        use crate::core::machine::{Probed, Provenance};
        let budget = Some(Probed::new(24_576, Provenance::EngineReported));
        assert_eq!(
            super::wired_cell(budget, None),
            "24576 MiB (engine-reported) (no floor configured; run checks the model's footprint)"
        );
        assert_eq!(
            super::wired_cell(budget, Some(150_000)),
            "24576 MiB (engine-reported) (need 150000 MB)"
        );
        assert_eq!(
            super::wired_cell(None, None),
            "unreadable (no floor configured; run checks the model's footprint)",
            "the user whose budget cannot be read is the one who most needs to know what run does"
        );
        assert_eq!(
            super::wired_cell(None, Some(150_000)),
            "unreadable (need 150000 MB)"
        );
    }
}
