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

fn status_rows(ctx: &Ctx) -> Result<Vec<Vec<String>>, ChekovError> {
    use crate::core::{checks, server};
    let reg = ctx.registry()?;
    let pid = server::live_pid(&ctx.config);
    let model = server::read_run_state(&ctx.config)
        .or_else(|| reg.active.clone())
        .unwrap_or_else(|| "none".to_owned());
    let (revision, ctx_size) = model_facts(&reg, &model);
    let required = ctx.config.file.limits.wired_limit_mb;
    let wired = checks::wired_limit_mb().map_or_else(
        || format!("unreadable (need {required} MB)"),
        |actual| format!("{actual} MB (need {required} MB)"),
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
}
