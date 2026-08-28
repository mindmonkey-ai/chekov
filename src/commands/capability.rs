//! `chekov capability` — doctor's twin for the machine rather than the server.
//!
//! Reports what this Mac is and what it can hold. Every number carries where
//! it came from, because the arithmetic rung is measurably 30.7 GiB low on a
//! 256 GiB M3 Ultra and a bare figure gives the reader no way to know that.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::core::machine::{self, Machine};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct CapabilityCmd {
    /// Emit the scan as JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

/// Human-readable scan. Pure so tests pin the contract.
#[must_use]
pub fn render_scan(m: &Machine) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let dash = || "-".to_owned();
    rows.push(vec!["chip".into(), m.chip.clone().unwrap_or_else(dash)]);
    rows.push(vec!["model".into(), m.model.clone().unwrap_or_else(dash)]);
    rows.push(vec![
        "memory".into(),
        m.memsize_bytes
            .map_or_else(dash, |b| format!("{} MiB", b / (1024 * 1024))),
    ]);
    rows.push(vec![
        "gpu cores".into(),
        m.gpu_cores.map_or_else(dash, |c| c.to_string()),
    ]);
    rows.push(vec![
        "perf threads".into(),
        m.perf_threads.map_or_else(dash, |c| c.to_string()),
    ]);
    rows.push(vec!["gpu budget".into(), render_budget(m)]);
    rows.push(vec!["macOS".into(), m.macos.clone().unwrap_or_else(dash)]);
    super::render_table(&["FIELD", "VALUE"], &rows)
}

/// The budget line, always naming its provenance — and naming the shortfall
/// when the engine and the formula disagree, which is the defect that
/// motivated this command.
fn render_budget(m: &Machine) -> String {
    let Some(budget) = m.budget else {
        return "unknown — run `chekov setup` so the engine can report it".to_owned();
    };
    let mut line = format!("{} MiB ({})", budget.value, budget.provenance.label());
    if let Some(bytes) = m.memsize_bytes {
        let (formula, _) = crate::core::checks::effective_wired_mb(0, bytes);
        if budget.value > formula {
            use std::fmt::Write;
            let _ = write!(
                line,
                " — {} MiB more than the {formula} MiB formula would predict",
                budget.value - formula
            );
        }
    }
    line
}

impl Command for CapabilityCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let m = machine::probe(&ctx.config.engine_dir());
        if self.json {
            println!("{}", render_json(&m));
        } else {
            println!("{}", render_scan(&m));
        }
        Ok(ExitCode::SUCCESS)
    }
}

/// Machine-readable scan. Provenance is a field, never dropped.
#[must_use]
pub fn render_json(m: &Machine) -> String {
    let budget = m
        .budget
        .map(|b| serde_json::json!({ "mib": b.value, "provenance": b.provenance.label() }));
    serde_json::json!({
        "chip": m.chip,
        "model": m.model,
        "memory_bytes": m.memsize_bytes,
        "gpu_cores": m.gpu_cores,
        "perf_threads": m.perf_threads,
        "gpu_budget": budget,
        "macos": m.macos,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::{Machine, render_json, render_scan};
    use crate::core::machine::{Probed, Provenance};

    fn m3_ultra(budget: Option<Probed<u64>>) -> Machine {
        Machine {
            chip: Some("Apple M3 Ultra".into()),
            model: Some("Mac15,14".into()),
            memsize_bytes: Some(274_877_906_944),
            gpu_cores: Some(80),
            perf_threads: Some(24),
            budget,
            macos: Some("27.0".into()),
        }
    }

    #[test]
    fn the_budget_line_names_its_provenance() {
        let out = render_scan(&m3_ultra(Some(Probed::new(
            228_065,
            Provenance::EngineReported,
        ))));
        assert!(out.contains("228065 MiB"), "{out}");
        assert!(
            out.contains("engine-reported"),
            "a bare number is the defect: {out}"
        );
    }

    #[test]
    fn the_scan_names_the_shortfall_the_formula_would_have_reported() {
        let out = render_scan(&m3_ultra(Some(Probed::new(
            228_065,
            Provenance::EngineReported,
        ))));
        assert!(
            out.contains("31457"),
            "the 30.7 GiB gap between engine and formula is the whole point: {out}"
        );
    }

    #[test]
    fn an_unknown_budget_names_its_remediation() {
        let out = render_scan(&m3_ultra(None));
        assert!(out.contains("chekov setup"), "{out}");
    }

    #[test]
    fn json_keeps_provenance_as_a_field() {
        let out = render_json(&m3_ultra(Some(Probed::new(196_608, Provenance::Predicted))));
        assert!(out.contains("\"provenance\":\"predicted\""), "{out}");
    }
}
