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

/// Geometry of the rendered grid, in user units.
const CELL_W: usize = 96;
const CELL_H: usize = 34;
const LEFT: usize = 250;
const TOP: usize = 96;

/// `bytes` as GiB with one decimal, without a lossy float cast.
fn gib(bytes: u64) -> String {
    let tenths = bytes * 10 / (1024 * 1024 * 1024);
    format!("{}.{} GiB", tenths / 10, tenths % 10)
}

/// XML-escape text that came from the registry or the machine.
///
/// A model name is user data; an unescaped `&` produces a file no viewer will
/// open.
fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Fill colours ordered by luminance, so the three states remain distinct in
/// greyscale. Colour is never the only carrier: each cell also prints its
/// glyph, and predicted inputs are hatched.
const fn fill_for(fit: Fit) -> &'static str {
    match fit {
        Fit::Fits => "#f2f9f2",
        Fit::Tight => "#fde9b8",
        Fit::Exceeds => "#e8a6a6",
        Fit::Unknown => "#ffffff",
    }
}

/// One cell in its place: which row, which context, which budget (§4).
struct CellAt<'a> {
    row: &'a Row,
    ctx: u32,
    cell: &'a Cell,
    budget_mib: u64,
}

/// The cell's own arithmetic, as the reader would check it by hand.
fn cell_title(at: &CellAt) -> String {
    let CellAt {
        row,
        ctx,
        cell,
        budget_mib,
    } = *at;
    let part = |v: Option<u64>| v.map_or_else(|| "unknown".to_owned(), gib);
    let verdict = match fit_for(cell.total_bytes(), budget_mib) {
        Fit::Fits => "fits",
        Fit::Tight => "tight (85-100% of budget)",
        Fit::Exceeds => "exceeds the budget",
        // Said in full: a blank here reads as a rendering gap rather than a
        // refusal to guess.
        Fit::Unknown => "unknown — an unknown input is never a fit",
    };
    format!(
        "{} {} @ {}: weights {} + kv {} ({}) + overhead {} ({}) = {} vs budget {} -> {}",
        row.name,
        row.quant,
        format_ctx(ctx),
        part(cell.weights_bytes),
        part(cell.kv_bytes.value),
        cell.kv_bytes.provenance.label(),
        part(cell.overhead_bytes.value),
        cell.overhead_bytes.provenance.label(),
        part(cell.total_bytes()),
        gib(budget_mib * 1024 * 1024),
        verdict,
    )
}

/// One cell: base fill, hatch when the inputs are predicted, glyph, tooltip.
fn svg_cell(row: &Row, index: usize, f: &Frontier) -> String {
    let cell = &row.cells[index];
    let ctx = f.ctx_ladder[index];
    let fit = fit_for(cell.total_bytes(), f.budget.value);
    let (x, y) = (LEFT + index * CELL_W, 0);
    let hatched = cell.inputs() != '#';
    let hatch = if hatched {
        format!(
            "<rect x=\"{x}\" y=\"{y}\" width=\"{}\" height=\"{}\" fill=\"url(#predicted)\"/>",
            CELL_W - 4,
            CELL_H - 4
        )
    } else {
        String::new()
    };
    format!(
        "<g><title>{}</title>\
         <rect x=\"{x}\" y=\"{y}\" width=\"{}\" height=\"{}\" fill=\"{}\" stroke=\"#666\"/>\
         {hatch}\
         <text x=\"{}\" y=\"{}\" text-anchor=\"middle\" font-size=\"14\">{} {}</text></g>",
        esc(&cell_title(&CellAt {
            row,
            ctx,
            cell,
            budget_mib: f.budget.value,
        })),
        CELL_W - 4,
        CELL_H - 4,
        fill_for(fit),
        x + (CELL_W - 4) / 2,
        y + 22,
        esc(&fit.glyph().to_string()),
        esc(&cell.inputs().to_string()),
    )
}

/// A self-contained SVG of the same frontier the terminal grid shows.
///
/// Hand-emitted: no dependency, no CDN, no script. The caller prints the path
/// and never opens it — launching a GUI from a CLI is an unrequested side
/// effect, and a printed path composes with the user's own tooling.
#[must_use]
pub fn render_svg(f: &Frontier) -> String {
    let width = LEFT + f.ctx_ladder.len() * CELL_W + 30;
    let height = TOP + f.rows.len() * CELL_H + 150;
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\" font-family=\"ui-monospace, Menlo, monospace\">\n\
         <rect width=\"{width}\" height=\"{height}\" fill=\"#ffffff\"/>\n\
         <defs><pattern id=\"predicted\" width=\"6\" height=\"6\" patternUnits=\"userSpaceOnUse\" \
         patternTransform=\"rotate(45)\">\
         <line x1=\"0\" y1=\"0\" x2=\"0\" y2=\"6\" stroke=\"#000000\" stroke-opacity=\"0.35\" \
         stroke-width=\"2\"/></pattern></defs>\n\
         <text x=\"20\" y=\"32\" font-size=\"18\">chekov capability frontier</text>\n"
    );
    out.push_str(&svg_header(f));
    out.push_str(&svg_axis(f));
    for (r, row) in f.rows.iter().enumerate() {
        out.push_str(&svg_row(row, r, f));
    }
    out.push_str(&svg_legend(f, height));
    out.push_str("</svg>\n");
    out
}

/// The budget line, and the same loud warning the terminal prints when the
/// ceiling every verdict is measured against is itself a guess.
fn svg_header(f: &Frontier) -> String {
    let mut out = format!(
        "<text x=\"20\" y=\"56\" font-size=\"13\">GPU budget {} MiB ({}) — {}</text>\n",
        f.budget.value,
        gib(f.budget.value * 1024 * 1024),
        esc(f.budget.provenance.label()),
    );
    if f.budget.provenance == Provenance::Predicted {
        out.push_str(
            "<text x=\"20\" y=\"76\" font-size=\"13\" fill=\"#a33\">CEILING PREDICTED — every \
             verdict below is measured against a guess</text>\n",
        );
    }
    out
}

fn svg_axis(f: &Frontier) -> String {
    let mut out = String::new();
    for (i, ctx) in f.ctx_ladder.iter().enumerate() {
        let _ = writeln!(
            out,
            "<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" font-size=\"12\">{}</text>",
            LEFT + i * CELL_W + (CELL_W - 4) / 2,
            TOP - 8,
            esc(&format_ctx(*ctx)),
        );
    }
    out
}

fn svg_row(row: &Row, index: usize, f: &Frontier) -> String {
    let y = TOP + index * CELL_H;
    let mut out = format!(
        "<g transform=\"translate(0,{y})\">\
         <text x=\"20\" y=\"22\" font-size=\"13\">{}</text>\
         <text x=\"{}\" y=\"22\" font-size=\"12\" fill=\"#555\">{}</text>",
        esc(&row.name),
        LEFT - 130,
        esc(&row.quant),
    );
    for i in 0..row.cells.len().min(f.ctx_ladder.len()) {
        out.push_str(&svg_cell(row, i, f));
    }
    out.push_str("</g>\n");
    out
}

/// The legend comes from the SAME function the terminal renderer calls, so
/// the two views can never disagree about what a glyph means.
fn svg_legend(f: &Frontier, height: usize) -> String {
    let base = TOP + f.rows.len() * CELL_H + 34;
    let mut out = String::new();
    for (i, line) in legend(f.budget).lines().enumerate() {
        let _ = writeln!(
            out,
            "<text x=\"20\" y=\"{}\" font-size=\"12\">{}</text>",
            base + i * 18,
            esc(line.trim()),
        );
    }
    let _ = writeln!(
        out,
        "<text x=\"20\" y=\"{}\" font-size=\"11\" fill=\"#555\">hatched cells have predicted \
         inputs; solid cells are measured. Hover a cell for its arithmetic.</text>",
        height - 20
    );
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
    fn the_svg_is_self_contained_and_never_reaches_out() {
        let svg = super::render_svg(&frontier(Provenance::EngineReported));
        assert!(svg.starts_with("<svg"), "{svg}");
        assert!(svg.trim_end().ends_with("</svg>"));
        assert!(svg.contains("xmlns=\"http://www.w3.org/2000/svg\""));
        // A report that phones home, runs script, or breaks without a network
        // is not a report you can put in a bug thread.
        assert!(!svg.contains("<script"), "no script: {svg}");
        assert!(!svg.contains("<image"), "no external image: {svg}");
        assert!(
            !svg.contains("href="),
            "no external reference of any kind: {svg}"
        );
    }

    #[test]
    fn predicted_inputs_are_hatched_so_the_distinction_survives_greyscale() {
        // Colour alone fails greyscale printing and colour-blind viewers, and
        // measured-vs-predicted is the whole point of the second character.
        let svg = super::render_svg(&frontier(Provenance::EngineReported));
        assert!(
            svg.contains("<pattern id=\"predicted\""),
            "the hatch must be defined: {svg}"
        );
        assert!(
            svg.contains("url(#predicted)"),
            "and applied to the predicted cells: {svg}"
        );
    }

    #[test]
    fn every_cell_carries_its_arithmetic_as_a_tooltip() {
        let svg = super::render_svg(&frontier(Provenance::EngineReported));
        assert!(
            svg.contains("<title>"),
            "a cell the reader cannot audit is a claim, not a measurement: {svg}"
        );
        assert!(
            svg.contains("weights") && svg.contains("+ kv") && svg.contains("vs budget"),
            "the tooltip shows the sum, not just the verdict: {svg}"
        );
    }

    #[test]
    fn an_unknown_cell_says_unknown_and_claims_no_fit() {
        // The second ctx column has no KV number.
        let svg = super::render_svg(&frontier(Provenance::EngineReported));
        assert!(svg.contains("unknown"), "{svg}");
    }

    #[test]
    fn the_svg_legend_is_the_ascii_legend() {
        // One legend function, so the two views cannot disagree about what a
        // glyph means.
        let predicted = super::render_svg(&frontier(Provenance::Predicted));
        assert!(
            predicted.contains("fits against a predicted ceiling"),
            "{predicted}"
        );
        assert!(predicted.contains("CEILING PREDICTED"), "{predicted}");
        let measured = super::render_svg(&frontier(Provenance::EngineReported));
        assert!(measured.contains("fits (&lt;85% of budget)"), "{measured}");
        assert!(!measured.contains("CEILING PREDICTED"), "{measured}");
    }

    #[test]
    fn text_from_the_registry_is_xml_escaped() {
        // A model name is user data; an unescaped & or < produces a file no
        // viewer will open.
        let mut f = frontier(Provenance::EngineReported);
        f.rows[0].name = "a&b<c>\"d\"".into();
        let svg = super::render_svg(&f);
        assert!(svg.contains("a&amp;b&lt;c&gt;"), "{svg}");
        assert!(
            !svg.contains("a&b<c>"),
            "the raw text must not survive: {svg}"
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
