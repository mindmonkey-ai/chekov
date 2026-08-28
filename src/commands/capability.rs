//! `chekov capability` — doctor's twin for the machine rather than the server.
//!
//! Reports what this Mac is and what it can hold. Every number carries where
//! it came from, because the arithmetic rung is measurably 30.7 GiB low on a
//! 256 GiB M3 Ultra and a bare figure gives the reader no way to know that.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::core::frontier;
use crate::core::machine::{self, Machine, Probed, Provenance};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct CapabilityCmd {
    #[command(subcommand)]
    pub action: Option<CapAction>,
    /// Emit the scan as JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, clap::Subcommand)]
pub enum CapAction {
    /// What this Mac is and what it can hold (the default).
    Scan,
    /// Grid of registered models against context lengths, with fit verdicts.
    Graph {
        /// Context lengths to plot; repeatable. Defaults to 32K/128K/256K.
        #[arg(long = "ctx")]
        ctx: Vec<u32>,
    },
    /// Rank what this machine should actually run, with rejections explained.
    Recommend {
        /// Context length to size against; defaults to the registry default.
        #[arg(long)]
        ctx: Option<u32>,
        /// `agent` weighs tool-call support; `chat` ignores it.
        #[arg(long, default_value = "agent")]
        role: String,
        /// Also query Hugging Face for candidates. The ONLY networked path;
        /// chekov never reaches out on an ordinary invocation.
        #[arg(long)]
        refresh: bool,
        /// How many discovered repos to size (each costs one request).
        #[arg(long, default_value_t = 12)]
        limit: u32,
    },
    /// Read one model's `GGUF` header and print its fit arithmetic line by line.
    Explain {
        /// Registered model name (defaults to the active model).
        name: Option<String>,
        /// Context length to size against; defaults to the model's `ctx_size`.
        #[arg(long)]
        ctx: Option<u32>,
    },
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
        match &self.action {
            Some(CapAction::Graph { ctx: ladder }) => return graph(ctx, ladder),
            Some(CapAction::Explain { name, ctx: at }) => {
                return explain(ctx, name.as_deref(), *at);
            }
            Some(CapAction::Recommend {
                ctx: at,
                role,
                refresh,
                limit,
            }) => {
                return recommend(
                    ctx,
                    &RecommendArgs {
                        at: *at,
                        role,
                        refresh: *refresh,
                        limit: *limit,
                    },
                );
            }
            _ => {}
        }
        let m = machine::probe(&ctx.config.engine_dir());
        if self.json {
            println!("{}", render_json(&m));
        } else {
            println!("{}", render_scan(&m));
        }
        Ok(ExitCode::SUCCESS)
    }
}

fn graph(ctx: &Ctx, ladder: &[u32]) -> Result<ExitCode, ChekovError> {
    let ladder = if ladder.is_empty() {
        vec![32_768, 131_072, 262_144]
    } else {
        ladder.to_vec()
    };
    let budget = machine::live_gpu_budget(&ctx.config.engine_dir()).ok_or_else(|| {
        ChekovError::SetupIncomplete {
            remaining: "the GPU budget is unknown — run `chekov setup` so the engine \
                        can report it"
                .to_owned(),
        }
    })?;
    let f = build_frontier(ctx, &ladder, budget)?;
    println!("{}", frontier::render_ascii(&f));
    Ok(ExitCode::SUCCESS)
}

/// Rows come from the registry; weights come from the files already on disk.
/// KV and overhead are an explicitly predicted reserve until the GGUF header
/// reader lands — an unknown must never render as a confident fit.
fn build_frontier(
    ctx: &Ctx,
    ladder: &[u32],
    budget: Probed<u64>,
) -> Result<frontier::Frontier, ChekovError> {
    let reg = ctx.registry()?;
    let mut rows: Vec<frontier::Row> = Vec::new();
    for (name, entry) in &reg.models {
        let weights = weights_on_disk(ctx, entry);
        // Real geometry when the header can be read; the coarse reserve only
        // when it cannot — and the cell says which, in its second character.
        let geometry = reg
            .effective(name)
            .ok()
            .map(|eff| crate::core::server::shard_path(&ctx.config, &eff))
            .filter(|p| p.exists())
            .and_then(|p| crate::core::gguf::read_geometry(&p).ok());
        let q8 = entry.extra_flags.iter().any(|f| f == "q8_0")
            || reg.defaults.flags.iter().any(|f| f == "q8_0");
        let cells = ladder
            .iter()
            .map(|&c| frontier::Cell {
                weights_bytes: weights,
                kv_bytes: geometry.as_ref().map_or_else(
                    || Probed::new(Some(kv_reserve(c)), Provenance::Predicted),
                    |g| {
                        crate::core::gguf::kv_bytes(g, c, q8).map_or_else(
                            || Probed::new(None, Provenance::Predicted),
                            |b| Probed::new(Some(b), Provenance::Measured),
                        )
                    },
                ),
                overhead_bytes: Probed::new(Some(3 * 1024 * 1024 * 1024), Provenance::Predicted),
            })
            .collect();
        rows.push(frontier::Row {
            name: name.clone(),
            quant: entry.quant.clone(),
            cells,
        });
    }
    rows.sort_by_key(|r| r.cells.first().and_then(|c| c.weights_bytes));
    Ok(frontier::Frontier {
        budget,
        ctx_ladder: ladder.to_vec(),
        rows,
    })
}

/// Bytes actually on disk for a model directory, or `None` when it is absent.
///
/// Walks one level of subdirectories: a repo like `unsloth/MiniMax-M2.7-GGUF`
/// keeps its shards under a quant folder (`UD-Q5_K_XL/…`), so a top-level-only
/// scan reports a fully downloaded 158 GiB model as absent.
fn weights_on_disk(ctx: &Ctx, entry: &crate::core::registry::ModelEntry) -> Option<u64> {
    let dir = ctx.config.root.join(&entry.path);
    let total = gguf_bytes_in(&dir) + subdir_gguf_bytes(&dir);
    (total > 0).then_some(total)
}

fn gguf_bytes_in(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "gguf"))
        .filter_map(|e| e.metadata().ok().map(|m| m.len()))
        .sum()
}

fn subdir_gguf_bytes(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| gguf_bytes_in(&e.path()))
        .sum()
}

/// A deliberately coarse KV reserve, labelled Predicted at the call site.
/// Real geometry needs the GGUF header, which slice 3 reads.
const fn kv_reserve(ctx_len: u32) -> u64 {
    (ctx_len as u64) * 40 * 8 * 128 * 2 * 17 / 16
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

/// Print the fit arithmetic for one model, sourced from its real GGUF header.
///
/// Local file read only — no network, and no seam change.
fn explain(ctx: &Ctx, name: Option<&str>, at: Option<u32>) -> Result<ExitCode, ChekovError> {
    let reg = ctx.registry()?;
    let name = match name {
        Some(n) => n.to_owned(),
        None => reg.active_name()?.to_owned(),
    };
    let eff = reg.effective(&name)?;
    let ctx_len = at.unwrap_or(eff.ctx_size);
    let shard = crate::core::server::shard_path(&ctx.config, &eff);
    let geometry = crate::core::gguf::read_geometry(&shard)?;
    let weights = weights_on_disk(ctx, &eff.entry);
    let q8 = eff.flags.iter().any(|f| f == "q8_0");
    println!(
        "{}",
        render_explain(&Explained {
            name: &name,
            geometry: &geometry,
            ctx_len,
            weights,
            q8_cache: q8,
        })
    );
    Ok(ExitCode::SUCCESS)
}

/// Everything `render_explain` needs, bundled so the renderer stays within the
/// 3-argument gate (§4).
pub struct Explained<'a> {
    pub name: &'a str,
    pub geometry: &'a crate::core::gguf::Geometry,
    pub ctx_len: u32,
    pub weights: Option<u64>,
    pub q8_cache: bool,
}

/// The layer-count ladder, which is where the 4x over-estimate hides.
fn render_geometry(g: &crate::core::gguf::Geometry) -> String {
    use crate::core::gguf;
    use std::fmt::Write;
    let unknown = || "?".to_owned();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "  block_count             {}",
        g.block_count.map_or_else(unknown, |v| v.to_string())
    );
    let _ = writeln!(
        out,
        "  nextn_predict_layers    {}",
        g.nextn_predict_layers.unwrap_or(0)
    );
    let _ = writeln!(
        out,
        "  full_attention_interval {}",
        g.full_attention_interval
            .map_or_else(|| "-".to_owned(), |v| v.to_string())
    );
    let _ = writeln!(
        out,
        "  kv_layers               {}   (NOT block_count)",
        gguf::kv_layers(g).map_or_else(unknown, |v| v.to_string())
    );
    out
}

/// The arithmetic, shown rather than asserted. Pure so tests pin it.
#[must_use]
pub fn render_explain(e: &Explained) -> String {
    use crate::core::gguf;
    use std::fmt::Write;
    let g = e.geometry;
    let unknown = || "?".to_owned();
    let show = |v: Option<u64>| v.map_or_else(unknown, |b| b.to_string());
    let mut out = format!("{}  (arch {})\n", e.name, g.arch);
    out.push_str(&render_geometry(g));
    let _ = writeln!(out, "  ctx (padded to 256)     {}", gguf::pad256(e.ctx_len));
    let _ = writeln!(
        out,
        "  kv cache type           {}",
        if e.q8_cache {
            "q8_0 (17/16 B per element)"
        } else {
            "f16 (2 B per element)"
        }
    );
    let kv = gguf::kv_bytes(g, e.ctx_len, e.q8_cache);
    let _ = writeln!(out, "  kv_bytes                {}", show(kv));
    let _ = writeln!(out, "  weights (on disk)       {}", show(e.weights));
    let total = e.weights.zip(kv).map(|(w, k)| w + k);
    let _ = writeln!(out, "  total (weights + kv)    {}", show(total));
    out
}

/// Rank the registered models for this machine, printing why each was rejected.
struct RecommendArgs<'a> {
    at: Option<u32>,
    role: &'a str,
    refresh: bool,
    limit: u32,
}

fn recommend(ctx: &Ctx, args: &RecommendArgs) -> Result<ExitCode, ChekovError> {
    use crate::core::recommend::{Role, rank};
    let (at, role) = (args.at, args.role);
    let role = match role {
        "chat" => Role::Chat,
        _ => Role::Agent,
    };
    let budget = machine::live_gpu_budget(&ctx.config.engine_dir()).ok_or_else(|| {
        ChekovError::SetupIncomplete {
            remaining: "the GPU budget is unknown — run `chekov setup`".to_owned(),
        }
    })?;
    let reg = ctx.registry()?;
    let ctx_len = at.unwrap_or(reg.defaults.ctx_size);
    let input = CandidateInput {
        ctx,
        reg: &reg,
        ctx_len,
    };
    let mut candidates: Vec<_> = reg
        .models
        .iter()
        .map(|(name, entry)| candidate_for(&input, name, entry))
        .collect();
    if args.refresh {
        eprintln!("chekov: querying Hugging Face (--refresh)…");
        candidates.extend(discovered(ctx, ctx_len, args.limit)?);
    } else {
        eprintln!("chekov: registered models only — pass --refresh to also query Hugging Face");
    }
    let ranked = rank(candidates, budget.value, role);
    println!("{}", render_recommend(&ranked, budget, ctx_len));
    Ok(ExitCode::SUCCESS)
}

/// Build one candidate from the registry plus whatever the disk can tell us.
struct CandidateInput<'a> {
    ctx: &'a Ctx,
    reg: &'a crate::core::registry::Registry,
    ctx_len: u32,
}

fn candidate_for(
    input: &CandidateInput,
    name: &str,
    entry: &crate::core::registry::ModelEntry,
) -> crate::core::recommend::Candidate {
    let (ctx, reg, ctx_len) = (input.ctx, input.reg, input.ctx_len);
    let weights = weights_on_disk(ctx, entry);
    let geometry = reg
        .effective(name)
        .ok()
        .map(|eff| crate::core::server::shard_path(&ctx.config, &eff))
        .filter(|p| p.exists())
        .and_then(|p| crate::core::gguf::read_geometry(&p).ok());
    let q8 = reg.defaults.flags.iter().any(|f| f == "q8_0");
    let kv = geometry
        .as_ref()
        .and_then(|g| crate::core::gguf::kv_bytes(g, ctx_len, q8));
    // Classify from the model's own embedded template, not from a guess.
    let parser = geometry
        .as_ref()
        .and_then(|g| g.chat_template.as_deref())
        .map_or(
            crate::core::toolparser::ToolParser::AutoparserFallthrough,
            crate::core::toolparser::classify,
        );
    crate::core::recommend::Candidate {
        name: name.to_owned(),
        quant: entry.quant.clone(),
        total_bytes: weights.zip(kv).map(|(w, k)| w + k),
        parser,
    }
}

/// The ranked list. Rejections are printed with their reason, never dropped.
#[must_use]
pub fn render_recommend(
    ranked: &[crate::core::recommend::Ranked],
    budget: Probed<u64>,
    ctx_len: u32,
) -> String {
    let width = ranked
        .iter()
        .map(|r| r.candidate.name.len())
        .max()
        .unwrap_or(20)
        .clamp(20, 52);
    let mut out = format!(
        "  at ctx {ctx_len}, budget {} MiB ({})\n           per repo: the largest quant whose size is fully known — best quality \
         that fits\n\n",
        budget.value,
        budget.provenance.label()
    );
    let mut rank = 0;
    for r in ranked {
        out.push_str(&render_row(r, &mut rank, width));
    }
    out
}

/// One row, ranked or rejected. Rejections keep their reason inline.
fn render_row(r: &crate::core::recommend::Ranked, rank: &mut u32, width: usize) -> String {
    use crate::core::recommend::Verdict;
    use std::fmt::Write;
    let mut out = String::new();
    match &r.verdict {
        Verdict::Ranked { notes } => {
            *rank += 1;
            let _ = writeln!(
                out,
                "  {rank:>2}. {:width$} {:>12}   {:7}  tools: {}",
                r.candidate.name,
                r.candidate.quant,
                fit_word(r.fit),
                r.candidate.parser.label()
            );
            for n in notes {
                let _ = writeln!(out, "       note: {n}");
            }
        }
        Verdict::Rejected { reason } => {
            let _ = writeln!(
                out,
                "   -  {:width$} {:>12}   rejected: {reason}",
                r.candidate.name, r.candidate.quant
            );
        }
    }
    out
}

const fn fit_word(fit: crate::core::frontier::Fit) -> &'static str {
    use crate::core::frontier::Fit;
    match fit {
        Fit::Fits => "fits",
        Fit::Tight => "tight",
        Fit::Exceeds => "exceeds",
        Fit::Unknown => "unknown",
    }
}

/// Candidates from the live HF list endpoint, sized through the same
/// `quant_options` path `pull` uses — so a repo that withholds a shard's size
/// yields `None` rather than a partial sum.
fn discovered(
    ctx: &Ctx,
    ctx_len: u32,
    limit: u32,
) -> Result<Vec<crate::core::recommend::Candidate>, ChekovError> {
    use crate::core::{catalog, hub, toolparser};
    let listed = catalog::discover(ctx.http.as_ref(), limit)?;
    let mut out = Vec::new();
    for row in listed.iter().filter(|r| catalog::worth_sizing(r)) {
        let Ok(repo) = crate::core::pullspec::RepoId::try_new(&row.id) else {
            continue;
        };
        let Ok(snapshot) = hub::fetch_snapshot(ctx.http.as_ref(), &repo, None) else {
            continue;
        };
        let parser = row
            .gguf
            .as_ref()
            .and_then(|g| g.chat_template.as_deref())
            .map_or(
                toolparser::ToolParser::AutoparserFallthrough,
                toolparser::classify,
            );
        // Largest quant whose weights are fully known — sizes are measured per
        // shard, never computed from a nominal bits-per-weight.
        let best = hub::quant_options(&snapshot)
            .into_iter()
            .filter(|o| o.bytes.is_some())
            .max_by_key(|o| o.bytes);
        let Some(best) = best else { continue };
        out.push(crate::core::recommend::Candidate {
            name: row.id.clone(),
            quant: best.tag,
            // KV is unknown without the header; the reserve keeps it honest.
            total_bytes: best.bytes.map(|w| w + kv_reserve(ctx_len)),
            parser,
        });
    }
    Ok(out)
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
