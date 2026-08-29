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

/// What the first character of a cell encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Metric {
    /// The memory verdict (the default).
    #[default]
    Fit,
    /// A band digit of the MEASURED decode rate; unmeasured cells stay `??`.
    TokS,
}

/// One stored measurement, attached to the cell it was taken in.
#[derive(Debug, Clone, PartialEq)]
pub struct Speed {
    pub decode: crate::core::stats::Summary,
    /// Prompt depth of the sweep row the median came from.
    pub depth: u32,
    pub run_id: String,
    /// The engine build that produced it — named when it is no longer current.
    pub engine_commit: String,
}

/// Band edges in tok/s. Fixed and absolute, never deciles of the peer set: a
/// digit must not move because a DIFFERENT model was benched (the §7.5 rule).
pub const SPEED_BAND_EDGES: [u32; 8] = [5, 10, 15, 20, 30, 40, 60, 80];

/// `1` below the first edge, `9` at or above the last.
#[must_use]
pub fn band_for(tok_s: f64) -> char {
    let edges_passed = SPEED_BAND_EDGES
        .iter()
        .filter(|&&edge| tok_s >= f64::from(edge))
        .count();
    let band = u32::try_from(edges_passed).unwrap_or(8) + 1;
    char::from_digit(band, 10).unwrap_or('9')
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
    /// A stored measurement for this exact model, quant and ctx, if any.
    pub speed: Option<Speed>,
}

impl Cell {
    /// The two characters: verdict or band digit, then inputs provenance.
    ///
    /// Under `TokS` a cell with no measurement or unknown geometry is `??` in
    /// full — a digit is never predicted, and a provenance mark beside a `?`
    /// would imply a measurement that does not exist.
    #[must_use]
    pub fn glyphs(&self, budget_mib: u64, metric: Metric) -> [char; 2] {
        let fit = fit_for(self.total_bytes(), budget_mib);
        match (metric, &self.speed) {
            (Metric::Fit, _) => [fit.glyph(), self.kv_inputs()],
            (Metric::TokS, Some(speed)) if fit != Fit::Unknown => {
                [band_for(speed.decode.median), self.kv_inputs()]
            }
            (Metric::TokS, _) => ['?', '?'],
        }
    }

    /// Sum of the parts, or `None` when any part is unknown.
    #[must_use]
    pub fn total_bytes(&self) -> Option<u64> {
        Some(self.weights_bytes? + self.kv_bytes.value? + self.overhead_bytes.value?)
    }

    /// Second character: was KV measured (GGUF header read), or predicted?
    ///
    /// KV only — deliberately. It is the term that varies with context and
    /// dominates the total; the overhead is a flat prediction in every cell,
    /// so folding it in would make every mark identical and carry nothing.
    /// The legend says exactly this, and the SVG tooltip prints each part's
    /// own provenance.
    #[must_use]
    pub const fn kv_inputs(&self) -> char {
        match self.kv_bytes.provenance {
            Provenance::Measured | Provenance::EngineReported => '#',
            Provenance::Predicted => '\u{b7}',
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
    pub metric: Metric,
    /// The engine build now installed; measured cells from another build are
    /// named in the footer.
    pub engine_commit: Option<String>,
    /// Numbered footnotes — anything excluded on the way in says so here.
    pub notes: Vec<String>,
}

/// The title line, naming the metric when it is not the default.
fn title(f: &Frontier) -> String {
    match f.metric {
        Metric::Fit => "chekov capability frontier".to_owned(),
        Metric::TokS => "chekov capability frontier   metric: decode tok/s (measured)".to_owned(),
    }
}

/// Rule 8: a measurement from a build that is no longer current is shown AND
/// named, never carried silently.
fn stale_line(f: &Frontier) -> Option<String> {
    if f.metric != Metric::TokS {
        return None;
    }
    let current = f.engine_commit.as_deref()?;
    let mut old: Vec<&str> = f
        .rows
        .iter()
        .flat_map(|row| &row.cells)
        .filter_map(|cell| cell.speed.as_ref())
        .map(|speed| speed.engine_commit.as_str())
        .filter(|commit| *commit != current)
        .collect();
    old.sort_unstable();
    old.dedup();
    if old.is_empty() {
        return None;
    }
    Some(format!(
        "measured cells are from build {}; the engine is now at {current}. Re-run 'chekov \
         capability bench' to revalidate.",
        old.join(", ")
    ))
}

/// Everything printed below the legend, in both renderers.
fn footer_lines(f: &Frontier) -> Vec<String> {
    let notes = f
        .notes
        .iter()
        .enumerate()
        .map(|(i, note)| format!("[{}] {note}", i + 1));
    stale_line(f).into_iter().chain(notes).collect()
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

/// `32K` for a multiple of 1024, else the bare number — one spelling for the
/// axis, the tooltips, and the footnotes.
#[must_use]
pub fn format_ctx(ctx: u32) -> String {
    if ctx.is_multiple_of(1024) {
        format!("{}K", ctx / 1024)
    } else {
        ctx.to_string()
    }
}

fn cell_row(row: &Row, f: &Frontier, name_width: usize) -> String {
    let mut line = format!("  {:name_width$}  {:>10}        ", row.name, row.quant);
    for cell in &row.cells {
        let [first, second] = cell.glyphs(f.budget.value, f.metric);
        let _ = write!(line, "{first:>7}{second}");
    }
    line
}

/// The band edges, generated from the same table `band_for` reads, so the
/// legend and the digits can never disagree.
fn band_legend() -> String {
    let first = SPEED_BAND_EDGES[0];
    let last = SPEED_BAND_EDGES[SPEED_BAND_EDGES.len() - 1];
    let middle = SPEED_BAND_EDGES
        .windows(2)
        .enumerate()
        .map(|(i, pair)| format!("{} {}-{}", i + 2, pair[0], pair[1]));
    let bands: Vec<String> = std::iter::once(format!("1 <{first}"))
        .chain(middle)
        .chain(std::iter::once(format!("9 >={last}")))
        .collect();
    format!(
        "    tok/s    {}\n             measured decode median at the deepest depth of the stored \
         run; no measurement = ??",
        bands.join("   ")
    )
}

/// Never suppressed: a two-character cell is unreadable without it.
fn legend(f: &Frontier) -> String {
    let fits = if f.budget.provenance == Provenance::Predicted {
        "fits against a predicted ceiling"
    } else {
        "fits (<85% of budget)"
    };
    let mut out = match f.metric {
        Metric::Fit => {
            format!("    fit      #  {fits}   +  tight (85-100%)   .  exceeds   ?  unknown\n")
        }
        Metric::TokS => format!("{}\n", band_legend()),
    };
    let _ = write!(
        out,
        "             inputs   #  kv measured   \u{b7}  kv predicted   ?  unknown   — {}",
        overhead_note(f)
    );
    out
}

/// What the second character does NOT cover, said in the legend: the
/// overhead's provenance. Today it is one flat prediction in every cell, and
/// the legend names the number; should that ever vary, the legend says so
/// rather than letting `#` pass for "all measured".
fn overhead_note(f: &Frontier) -> String {
    let overheads: Vec<Probed<Option<u64>>> = f
        .rows
        .iter()
        .flat_map(|row| row.cells.iter().map(|cell| cell.overhead_bytes))
        .collect();
    let all_predicted = overheads
        .iter()
        .all(|o| o.provenance == Provenance::Predicted);
    let flat = overheads.first().and_then(|first| {
        overheads
            .iter()
            .all(|o| o.value == first.value)
            .then_some(first.value)
            .flatten()
    });
    match (all_predicted, flat) {
        (true, Some(bytes)) => format!("overhead is a flat predicted {} in every cell", gib(bytes)),
        (true, None) => "overhead is predicted in every cell".to_owned(),
        (false, _) => "overhead provenance varies by cell (hover the SVG cells)".to_owned(),
    }
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
    let mut out = format!("  {}\n", title(f));
    out.push_str(&budget_header(f.budget));
    out.push_str("\n\n");
    out.push_str(&axis_line(&f.ctx_ladder, name_width));
    out.push('\n');
    for row in &f.rows {
        out.push_str(&cell_row(row, f, name_width));
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&legend(f));
    for line in footer_lines(f) {
        let _ = write!(out, "\n    {line}");
    }
    out
}

/// Geometry of the rendered grid, in user units.
const CELL_W: usize = 96;
const CELL_H: usize = 34;
const TOP: usize = 96;
const MARGIN: usize = 20;
const GAP: usize = 16;
const LINE_H: usize = 18;

const CEILING_WARNING: &str = "CEILING PREDICTED — every verdict below is measured against a guess";

const FOOTER: [&str; 2] = [
    "hatched cells carry predicted inputs; solid cells are measured.",
    "hover a cell for its arithmetic.",
];

/// Advance of `chars` glyphs in a monospace face at `font_px`. Chrome renders
/// `ui-monospace` at 0.60 em (measured with `getBBox`); 0.65 plus one glyph of
/// slack leaves room for a wider fallback face. Right-side whitespace is
/// cheap; an estimate that undercuts the text loses words. (A `qlmanage`
/// thumbnail ignores the canvas width entirely and is not a check.)
const fn text_w(chars: usize, font_px: usize) -> usize {
    chars * font_px * 13 / 20 + font_px
}

/// Positions derived from the frontier's own content. A long model name moves
/// the quant column and the grid right instead of running under them, and the
/// canvas is at least as wide as its widest line of text, so nothing is
/// clipped at the right edge.
struct Layout {
    quant_x: usize,
    left: usize,
    width: usize,
    legend_top: usize,
    height: usize,
}

fn longest(rows: &[Row], text: impl Fn(&Row) -> &str) -> usize {
    rows.iter()
        .map(|row| text(row).chars().count())
        .max()
        .unwrap_or(0)
}

fn layout(f: &Frontier) -> Layout {
    let quant_x = MARGIN + text_w(longest(&f.rows, |row| &row.name), 13) + GAP;
    let left = quant_x + text_w(longest(&f.rows, |row| &row.quant), 12) + GAP;
    let grid_w = left + f.ctx_ladder.len() * CELL_W + MARGIN;
    let legend_top = TOP + f.rows.len() * CELL_H + 2 * LINE_H;
    let text_lines = legend(f).lines().count() + footer_lines(f).len() + FOOTER.len();
    Layout {
        quant_x,
        left,
        width: grid_w.max(widest_text(f)),
        legend_top,
        height: legend_top + text_lines * LINE_H + MARGIN,
    }
}

/// The widest line of prose on the sheet, margins included.
fn widest_text(f: &Frontier) -> usize {
    let legend = legend(f);
    let footer = footer_lines(f);
    let prose = legend
        .lines()
        .map(|line| text_w(line.trim().chars().count(), 12))
        .chain(footer.iter().map(|line| text_w(line.chars().count(), 12)))
        .chain(FOOTER.iter().map(|line| text_w(line.chars().count(), 11)))
        .chain([
            text_w(title(f).chars().count(), 18),
            text_w(header_line(f).chars().count(), 13),
            text_w(CEILING_WARNING.chars().count(), 13),
        ]);
    prose.max().unwrap_or(0) + 2 * MARGIN
}

/// The frontier with its layout resolved: what every emitter draws from.
struct Sheet<'a> {
    f: &'a Frontier,
    lay: Layout,
}

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
        "{} {} @ {}: weights {} + kv {} ({}) + overhead {} ({}) = {} vs budget {} -> {}{}",
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
        speed_note(cell),
    )
}

/// The measurement behind a band digit, with the spread and the run it came
/// from — the audit trail the digit compresses away.
fn speed_note(cell: &Cell) -> String {
    cell.speed.as_ref().map_or_else(String::new, |s| {
        format!(
            "; decode {:.1} tok/s [{:.1}..{:.1}] at depth {}, run {}",
            s.decode.median, s.decode.p10, s.decode.p90, s.depth, s.run_id
        )
    })
}

/// One cell: base fill, hatch when the inputs are predicted, glyph, tooltip.
fn svg_cell(row: &Row, index: usize, sheet: &Sheet) -> String {
    let f = sheet.f;
    let cell = &row.cells[index];
    let ctx = f.ctx_ladder[index];
    let fit = fit_for(cell.total_bytes(), f.budget.value);
    let (x, y) = (sheet.lay.left + index * CELL_W, 0);
    let [first, second] = cell.glyphs(f.budget.value, f.metric);
    let hatched = cell.kv_inputs() != '#';
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
        esc(&first.to_string()),
        esc(&second.to_string()),
    )
}

/// A self-contained SVG of the same frontier the terminal grid shows.
///
/// Hand-emitted: no dependency, no CDN, no script. The caller prints the path
/// and never opens it — launching a GUI from a CLI is an unrequested side
/// effect, and a printed path composes with the user's own tooling.
#[must_use]
pub fn render_svg(f: &Frontier) -> String {
    let sheet = Sheet { f, lay: layout(f) };
    let (width, height) = (sheet.lay.width, sheet.lay.height);
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\" font-family=\"ui-monospace, Menlo, monospace\">\n\
         <rect width=\"{width}\" height=\"{height}\" fill=\"#ffffff\"/>\n\
         <defs><pattern id=\"predicted\" width=\"6\" height=\"6\" patternUnits=\"userSpaceOnUse\" \
         patternTransform=\"rotate(45)\">\
         <line x1=\"0\" y1=\"0\" x2=\"0\" y2=\"6\" stroke=\"#000000\" stroke-opacity=\"0.35\" \
         stroke-width=\"2\"/></pattern></defs>\n\
         <text x=\"20\" y=\"32\" font-size=\"18\" xml:space=\"preserve\">{}</text>\n",
        esc(&title(f)),
    );
    out.push_str(&svg_header(f));
    out.push_str(&svg_axis(&sheet));
    for (r, row) in f.rows.iter().enumerate() {
        out.push_str(&svg_row(row, r, &sheet));
    }
    out.push_str(&svg_legend(&sheet));
    out.push_str("</svg>\n");
    out
}

fn header_line(f: &Frontier) -> String {
    format!(
        "GPU budget {} MiB ({}) — {}",
        f.budget.value,
        gib(f.budget.value * 1024 * 1024),
        f.budget.provenance.label(),
    )
}

/// The budget line, and the same loud warning the terminal prints when the
/// ceiling every verdict is measured against is itself a guess.
fn svg_header(f: &Frontier) -> String {
    let mut out = format!(
        "<text x=\"{MARGIN}\" y=\"56\" font-size=\"13\">{}</text>\n",
        esc(&header_line(f)),
    );
    if f.budget.provenance == Provenance::Predicted {
        let _ = writeln!(
            out,
            "<text x=\"{MARGIN}\" y=\"76\" font-size=\"13\" fill=\"#a33\">{}</text>",
            esc(CEILING_WARNING),
        );
    }
    out
}

fn svg_axis(sheet: &Sheet) -> String {
    let mut out = String::new();
    for (i, ctx) in sheet.f.ctx_ladder.iter().enumerate() {
        let _ = writeln!(
            out,
            "<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" font-size=\"12\">{}</text>",
            sheet.lay.left + i * CELL_W + (CELL_W - 4) / 2,
            TOP - 8,
            esc(&format_ctx(*ctx)),
        );
    }
    out
}

fn svg_row(row: &Row, index: usize, sheet: &Sheet) -> String {
    let y = TOP + index * CELL_H;
    let mut out = format!(
        "<g transform=\"translate(0,{y})\">\
         <text x=\"{MARGIN}\" y=\"22\" font-size=\"13\">{}</text>\
         <text x=\"{}\" y=\"22\" font-size=\"12\" fill=\"#555\">{}</text>",
        esc(&row.name),
        sheet.lay.quant_x,
        esc(&row.quant),
    );
    for i in 0..row.cells.len().min(sheet.f.ctx_ladder.len()) {
        out.push_str(&svg_cell(row, i, sheet));
    }
    out.push_str("</g>\n");
    out
}

/// The legend comes from the SAME function the terminal renderer calls, so
/// the two views can never disagree about what a glyph means. Its spacing is
/// preserved — SVG collapses runs of spaces by default, which would fuse the
/// legend's columns into one run-on line.
fn svg_legend(sheet: &Sheet) -> String {
    let mut out = String::new();
    let mut y = sheet.lay.legend_top;
    for line in legend(sheet.f).lines() {
        let _ = writeln!(
            out,
            "<text x=\"{MARGIN}\" y=\"{y}\" font-size=\"12\" xml:space=\"preserve\">{}</text>",
            esc(line.trim()),
        );
        y += LINE_H;
    }
    for line in footer_lines(sheet.f) {
        let _ = writeln!(
            out,
            "<text x=\"{MARGIN}\" y=\"{y}\" font-size=\"12\" fill=\"#a33\">{}</text>",
            esc(&line),
        );
        y += LINE_H;
    }
    for line in FOOTER {
        let _ = writeln!(
            out,
            "<text x=\"{MARGIN}\" y=\"{y}\" font-size=\"11\" fill=\"#555\">{}</text>",
            esc(line),
        );
        y += LINE_H;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Cell, Fit, Metric, fit_for};
    use crate::core::machine::{Probed, Provenance};

    const GIB: u64 = 1024 * 1024 * 1024;

    fn cell(weights: Option<u64>, kv: Option<u64>) -> Cell {
        Cell {
            weights_bytes: weights,
            kv_bytes: Probed::new(kv, Provenance::Predicted),
            overhead_bytes: Probed::new(Some(GIB / 4), Provenance::Predicted),
            speed: None,
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
            metric: Metric::Fit,
            engine_commit: None,
            notes: Vec::new(),
        }
    }

    fn measured(tok_s: f64) -> super::Speed {
        super::Speed {
            decode: crate::core::stats::Summary {
                median: tok_s,
                p10: tok_s - 1.0,
                p90: tok_s + 1.0,
                n: 4,
                warmup_dropped: 1,
            },
            depth: 16_384,
            run_id: "20260828T040614Z-qwen3.8-27b".into(),
            engine_commit: "dda1b0d67".into(),
        }
    }

    /// Two known-fit cells: the first measured, the second not.
    fn tok_s_frontier() -> super::Frontier {
        let mut f = frontier(Provenance::EngineReported);
        f.metric = Metric::TokS;
        f.engine_commit = Some("dda1b0d67".into());
        f.rows[0].cells[0].speed = Some(measured(68.1));
        f.rows[0].cells[1] = cell(Some(24 * GIB), Some(GIB));
        f
    }

    fn grid_row(out: &str) -> String {
        out.lines()
            .find(|l| l.contains("qwen3.8-27b"))
            .expect("the model's row")
            .to_owned()
    }

    #[test]
    fn speed_bands_have_fixed_edges_so_a_peer_bench_cannot_move_a_digit() {
        assert_eq!(super::band_for(4.9), '1');
        assert_eq!(super::band_for(5.0), '2');
        assert_eq!(super::band_for(29.9), '5');
        assert_eq!(super::band_for(79.9), '8');
        assert_eq!(super::band_for(80.0), '9');
        assert_eq!(super::band_for(500.0), '9');
    }

    #[test]
    fn under_tok_s_a_measured_cell_is_a_band_digit_and_an_unmeasured_one_stays_unknown() {
        let out = super::render_ascii(&tok_s_frontier());
        let row = grid_row(&out);
        assert!(row.contains("8\u{b7}"), "digit + inputs provenance: {row}");
        assert!(
            row.trim_end().ends_with("??"),
            "no measurement, no digit: {row}"
        );
        assert!(
            out.contains("1 <5") && out.contains("9 >=80"),
            "edges: {out}"
        );
        assert!(
            out.contains("deepest depth"),
            "which depth the median is from: {out}"
        );
    }

    #[test]
    fn a_measured_cell_with_unknown_geometry_never_becomes_a_digit() {
        let mut f = tok_s_frontier();
        f.rows[0].cells[1] = cell(Some(24 * GIB), None);
        f.rows[0].cells[1].speed = Some(measured(68.1));
        let row = grid_row(&super::render_ascii(&f));
        assert!(row.trim_end().ends_with("??"), "{row}");
    }

    #[test]
    fn under_fit_a_stored_speed_changes_nothing() {
        let mut f = tok_s_frontier();
        f.metric = Metric::Fit;
        let out = super::render_ascii(&f);
        let row = grid_row(&out);
        // The model name carries an `8` of its own; look only at the cells.
        let cells = row
            .rsplit("UD-Q6_K_XL")
            .next()
            .expect("cells after the quant");
        assert!(cells.contains("#\u{b7}"), "{row}");
        assert!(
            !cells.chars().any(|c| c.is_ascii_digit()),
            "no band digit under fit: {row}"
        );
        assert!(!out.contains("tok/s"), "{out}");
    }

    #[test]
    fn a_stale_build_is_named_in_the_footer_never_carried_silently() {
        let mut f = tok_s_frontier();
        f.engine_commit = Some("f00dbabe1".into());
        let out = super::render_ascii(&f);
        assert!(
            out.contains(
                "measured cells are from build dda1b0d67; the engine is now at f00dbabe1. \
                 Re-run 'chekov capability bench' to revalidate."
            ),
            "{out}"
        );
        let current = super::render_ascii(&tok_s_frontier());
        assert!(
            !current.contains("measured cells are from build"),
            "{current}"
        );
    }

    #[test]
    fn notes_are_numbered_footnotes_below_the_grid() {
        let mut f = tok_s_frontier();
        f.notes
            .push("eval/broken could not be read: line 3: bad json — excluded".into());
        let out = super::render_ascii(&f);
        assert!(out.contains("[1] eval/broken could not be read"), "{out}");
    }

    #[test]
    fn the_svg_carries_the_band_digit_and_the_measurement_in_the_tooltip() {
        let svg = super::render_svg(&tok_s_frontier());
        assert!(svg.contains(">8 \u{b7}<"), "digit cell: {svg}");
        assert!(svg.contains(">? ?<"), "unmeasured cell: {svg}");
        assert!(
            svg.contains("decode 68.1 tok/s [67.1..69.1] at depth 16384, run 20260828T040614Z"),
            "tooltip: {svg}"
        );
        assert!(
            svg.contains("1 &lt;5"),
            "edges reach the SVG legend too: {svg}"
        );
    }

    #[test]
    fn the_svg_prints_the_stale_footer_from_the_same_source() {
        let mut f = tok_s_frontier();
        f.engine_commit = Some("f00dbabe1".into());
        let svg = super::render_svg(&f);
        assert!(
            svg.contains("measured cells are from build dda1b0d67"),
            "{svg}"
        );
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
    fn a_long_model_name_moves_the_grid_right_instead_of_running_under_the_quant() {
        // Seen live: an 18-character name at a fixed quant column printed
        // "ornith-1.5-35b-a3b" straight through "Q8_0".
        let short = super::layout(&frontier(Provenance::EngineReported));
        let mut f = frontier(Provenance::EngineReported);
        f.rows[0].name = "a".repeat(40);
        let long = super::layout(&f);
        assert!(
            long.quant_x > short.quant_x,
            "{} vs {}",
            long.quant_x,
            short.quant_x
        );
        assert!(long.quant_x >= super::MARGIN + super::text_w(40, 13));
        assert!(long.left > long.quant_x);
        let svg = super::render_svg(&f);
        assert!(svg.contains(&format!("x=\"{}\"", long.quant_x)), "{svg}");
    }

    #[test]
    fn the_canvas_is_at_least_as_wide_as_its_widest_line_of_text() {
        // Seen live: the footer ran off the right edge of a three-column
        // grid, because the width came from the grid alone.
        let mut f = frontier(Provenance::Predicted);
        f.ctx_ladder.truncate(1);
        f.rows[0].cells.truncate(1);
        let lay = super::layout(&f);
        let widest_legend = super::legend(&f)
            .lines()
            .map(|line| line.trim().chars().count())
            .max()
            .unwrap_or(0);
        assert!(lay.width >= 2 * super::MARGIN + super::text_w(widest_legend, 12));
        assert!(
            lay.width
                >= 2 * super::MARGIN + super::text_w(super::CEILING_WARNING.chars().count(), 13)
        );
        let svg = super::render_svg(&f);
        assert!(svg.contains(&format!("width=\"{}\"", lay.width)), "{svg}");
        assert!(
            svg.contains("xml:space=\"preserve\""),
            "legend spacing survives: {svg}"
        );
    }

    #[test]
    fn the_inputs_legend_says_what_the_glyph_encodes_and_names_the_guessed_overhead() {
        // The second character reports KV's provenance only; the overhead is a
        // flat prediction in every cell. A legend that says "measured" for a
        // sum with a guessed summand is the lie; the fix is to say exactly
        // what the glyph encodes and what is guessed everywhere.
        let f = frontier(Provenance::EngineReported);
        let out = super::render_ascii(&f);
        assert!(out.contains("#  kv measured"), "{out}");
        assert!(out.contains("\u{b7}  kv predicted"), "{out}");
        assert!(
            out.contains("overhead is a flat predicted 0.2 GiB in every cell"),
            "{out}"
        );
        assert!(
            !out.contains("#  measured   "),
            "the old wording is gone: {out}"
        );
        // One legend function feeds both renderers.
        let svg = super::render_svg(&f);
        assert!(
            svg.contains("overhead is a flat predicted 0.2 GiB in every cell"),
            "{svg}"
        );
        assert_eq!(f.rows[0].cells[0].kv_inputs(), '\u{b7}');
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
