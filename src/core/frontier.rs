//! The model x context frontier: what this machine can actually hold.
//!
//! One data model, rendered once here as a terminal grid, so no two views can
//! disagree.
//!
//! Every cell carries a fit verdict and the provenance of its inputs.
//!
//! One glyph cannot carry two orthogonal facts, so each cell is two
//! characters: the verdict, then where the numbers came from.

use std::fmt::Write;

use crate::core::machine::{Probed, Provenance};

/// Fraction of the budget at which a model stops leaving headroom.
///
/// Shared with `hub::render_quant_table` so the quant table and the frontier
/// can never disagree about what "tight" means.
pub const TIGHT_FRACTION_PCT: u64 = 85;

/// The arithmetic verdict for one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Fits,
    Tight,
    Exceeds,
    /// An input was unknown. Never "large" — unknown.
    Unknown,
}

impl Fit {
    #[must_use]
    pub const fn glyph(self) -> char {
        match self {
            Self::Fits => '#',
            Self::Tight => '+',
            Self::Exceeds => '.',
            Self::Unknown => '?',
        }
    }
}

/// Fit of `total` against `budget_mib`.
///
/// `None` in means `Unknown` out — a missing component can never be silently
/// treated as zero, which would turn an unknown into a confident "fits".
#[must_use]
pub const fn fit_for(total_bytes: Option<u64>, budget_mib: u64) -> Fit {
    let Some(total) = total_bytes else {
        return Fit::Unknown;
    };
    let budget = budget_mib * 1024 * 1024;
    if total > budget {
        Fit::Exceeds
    } else if total * 100 >= budget * TIGHT_FRACTION_PCT {
        Fit::Tight
    } else {
        Fit::Fits
    }
}

/// One model at one context length.
#[derive(Debug, Clone)]
pub struct Cell {
    pub weights_bytes: Option<u64>,
    pub kv_bytes: Probed<Option<u64>>,
    pub overhead_bytes: Probed<Option<u64>>,
}

impl Cell {
    /// Sum of the parts, or `None` when any part is unknown.
    #[must_use]
    pub fn total_bytes(&self) -> Option<u64> {
        Some(self.weights_bytes? + self.kv_bytes.value? + self.overhead_bytes.value?)
    }

    /// Second character: are the inputs measured, or predicted?
    #[must_use]
    pub const fn inputs(&self) -> char {
        match (self.kv_bytes.provenance, self.overhead_bytes.provenance) {
            (Provenance::Measured | Provenance::EngineReported, _) => '#',
            _ => '\u{b7}',
        }
    }
}

/// One model row: the candidate plus its cell per context length.
#[derive(Debug, Clone)]
pub struct Row {
    pub name: String,
    pub quant: String,
    pub cells: Vec<Cell>,
}

/// The whole grid, computed once.
#[derive(Debug, Clone)]
pub struct Frontier {
    pub budget: Probed<u64>,
    pub ctx_ladder: Vec<u32>,
    pub rows: Vec<Row>,
}

/// Header naming the budget and, loudly, when the ceiling itself is a guess.
fn budget_header(budget: Probed<u64>) -> String {
    // MiB -> GiB with one decimal, without a lossy f64 cast.
    let gib_tenths = budget.value * 10 / 1024;
    let mut head = format!(
        "  GPU budget   {} MiB ({}.{} GiB)   {}",
        budget.value,
        gib_tenths / 10,
        gib_tenths % 10,
        budget.provenance.label()
    );
    if budget.provenance == Provenance::Predicted {
        head.push_str("\n  CEILING PREDICTED — every verdict below is measured against a guess");
    }
    head
}

fn axis_line(ladder: &[u32], name_width: usize) -> String {
    let mut line = format!("{:name_width$}  {:>10}   ctx \u{2192}", "", "");
    for ctx in ladder {
        let _ = write!(line, "{:>8}", format_ctx(*ctx));
    }
    line
}

fn format_ctx(ctx: u32) -> String {
    if ctx.is_multiple_of(1024) {
        format!("{}K", ctx / 1024)
    } else {
        ctx.to_string()
    }
}

fn cell_row(row: &Row, budget_mib: u64, name_width: usize) -> String {
    let mut line = format!("  {:name_width$}  {:>10}        ", row.name, row.quant);
    for cell in &row.cells {
        let fit = fit_for(cell.total_bytes(), budget_mib).glyph();
        let _ = write!(line, "{:>7}{}", fit, cell.inputs());
    }
    line
}

/// Never suppressed: a two-character cell is unreadable without it.
fn legend(budget: Probed<u64>) -> String {
    let fits = if budget.provenance == Provenance::Predicted {
        "fits against a predicted ceiling"
    } else {
        "fits (<85% of budget)"
    };
    format!(
        "    fit      #  {fits}   +  tight (85-100%)   .  exceeds   ?  unknown\n             inputs   #  measured   \u{b7}  predicted   ?  unknown"
    )
}

/// The terminal grid.
#[must_use]
pub fn render_ascii(f: &Frontier) -> String {
    let name_width = f
        .rows
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let mut out = String::from("  chekov capability frontier\n");
    out.push_str(&budget_header(f.budget));
    out.push_str("\n\n");
    out.push_str(&axis_line(&f.ctx_ladder, name_width));
    out.push('\n');
    for row in &f.rows {
        out.push_str(&cell_row(row, f.budget.value, name_width));
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&legend(f.budget));
    out
}

#[cfg(test)]
mod tests {
    use super::{Cell, Fit, fit_for};
    use crate::core::machine::{Probed, Provenance};

    const GIB: u64 = 1024 * 1024 * 1024;

    fn cell(weights: Option<u64>, kv: Option<u64>) -> Cell {
        Cell {
            weights_bytes: weights,
            kv_bytes: Probed::new(kv, Provenance::Predicted),
            overhead_bytes: Probed::new(Some(GIB / 4), Provenance::Predicted),
        }
    }

    fn frontier(prov: Provenance) -> super::Frontier {
        super::Frontier {
            budget: Probed::new(222_720, prov),
            ctx_ladder: vec![32_768, 131_072],
            rows: vec![super::Row {
                name: "qwen3.8-27b".into(),
                quant: "UD-Q6_K_XL".into(),
                cells: vec![cell(Some(24 * GIB), Some(GIB)), cell(Some(24 * GIB), None)],
            }],
        }
    }

    #[test]
    fn a_predicted_ceiling_is_announced_and_changes_the_legend() {
        let out = super::render_ascii(&frontier(Provenance::Predicted));
        assert!(
            out.contains("CEILING PREDICTED"),
            "every verdict is measured against this number; if it is a guess, say so: {out}"
        );
        assert!(
            out.contains("fits against a predicted ceiling"),
            "the legend must not promise a plain `fits` on a guessed budget: {out}"
        );
    }

    #[test]
    fn a_measured_ceiling_says_nothing_extra() {
        let out = super::render_ascii(&frontier(Provenance::EngineReported));
        assert!(!out.contains("CEILING PREDICTED"), "{out}");
        assert!(out.contains("fits (<85% of budget)"), "{out}");
    }

    #[test]
    fn a_cell_with_unknown_geometry_renders_as_unknown_not_as_a_fit() {
        let out = super::render_ascii(&frontier(Provenance::EngineReported));
        assert!(
            out.contains('?'),
            "the second ctx column has no KV number and must show `?`: {out}"
        );
    }

    #[test]
    fn the_legend_is_always_present() {
        let out = super::render_ascii(&frontier(Provenance::EngineReported));
        assert!(
            out.contains("inputs   #"),
            "a two-char cell is unreadable without it: {out}"
        );
    }

    #[test]
    fn an_unknown_component_makes_the_cell_unknown_never_fitting() {
        let c = cell(Some(10 * GIB), None);
        assert_eq!(c.total_bytes(), None, "a missing part is not zero");
        assert_eq!(
            fit_for(c.total_bytes(), 222_720),
            Fit::Unknown,
            "an unknown input must never render as a confident fit"
        );
    }

    #[test]
    fn a_comfortable_model_fits() {
        // 36 GiB against a 222720 MiB (217.5 GiB) budget.
        assert_eq!(fit_for(Some(36 * GIB), 222_720), Fit::Fits);
    }

    #[test]
    fn the_tight_band_starts_at_85_percent_of_budget() {
        let budget_mib = 1000_u64;
        let budget_bytes = budget_mib * 1024 * 1024;
        assert_eq!(
            fit_for(Some(budget_bytes * 84 / 100), budget_mib),
            Fit::Fits
        );
        assert_eq!(
            fit_for(Some(budget_bytes * 85 / 100), budget_mib),
            Fit::Tight,
            "85% is the documented edge and belongs to tight"
        );
        assert_eq!(
            fit_for(Some(budget_bytes * 99 / 100), budget_mib),
            Fit::Tight
        );
    }

    #[test]
    fn over_budget_exceeds() {
        let budget_mib = 1000_u64;
        assert_eq!(
            fit_for(Some(budget_mib * 1024 * 1024 + 1), budget_mib),
            Fit::Exceeds
        );
    }
}
