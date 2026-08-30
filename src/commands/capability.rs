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
    Graph(GraphOpts),
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
    /// Measure candidates through chekov's own translator; store each run.
    Bench(BenchOpts),
    /// Compare two stored bench runs (same environment only).
    Compare {
        /// Run ids under `eval/`, or paths to run directories.
        a: std::path::PathBuf,
        b: std::path::PathBuf,
    },
}

#[derive(Debug, clap::Args)]
pub struct GraphOpts {
    /// Context lengths to plot; repeatable. Defaults to 32K/128K/256K.
    #[arg(long = "ctx")]
    pub ctx: Vec<u32>,
    /// Also write a self-contained SVG. Bare, it lands under `reports/`. The
    /// path is PRINTED, never opened — launching a GUI from a CLI is an
    /// unrequested side effect.
    #[arg(long, num_args = 0..=1)]
    pub svg: Option<Option<std::path::PathBuf>>,
    /// What the first cell character encodes: `fit` (memory verdict) or
    /// `tok-s` (a band digit of the MEASURED decode median from stored bench
    /// runs; a cell with no run stays `??`).
    #[arg(long, value_enum, default_value_t = MetricArg::Fit)]
    pub metric: MetricArg,
}

/// `--metric`, exactly the spec's two values (§2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum MetricArg {
    #[default]
    Fit,
    #[value(name = "tok-s")]
    TokS,
}

impl From<MetricArg> for frontier::Metric {
    fn from(arg: MetricArg) -> Self {
        match arg {
            MetricArg::Fit => Self::Fit,
            MetricArg::TokS => Self::TokS,
        }
    }
}

#[derive(Debug, clap::Args)]
pub struct BenchOpts {
    /// Graded probe set (TOML). There is no compiled-in fixture yet:
    /// fixture-v1 is release-gated on a three-model measurement campaign.
    #[arg(long)]
    pub fixture: Option<std::path::PathBuf>,
    /// Continue a previous run id, skipping tasks its JSONL already holds.
    #[arg(long)]
    pub resume: Option<String>,
    /// Registered models to bench sequentially (default: the active one).
    /// Bench launches and tears each down; it never stops a server it did
    /// not start.
    #[arg(long, value_delimiter = ',')]
    pub models: Vec<String>,
    /// Print the plan and the wall-clock estimate; run nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip the confirm gate that any launch step requires.
    #[arg(long)]
    pub yes: bool,
    /// Which task sets to measure. Default `throughput`; unset with
    /// `--codebase` means only the codebase set.
    #[arg(long, value_enum)]
    pub suite: Option<crate::core::bench::lifecycle::Suite>,
    /// The user's own Rust repository as graded infill tasks (spec §8, slice
    /// A). Refuses a dirty tree; reads from a detached worktree. Given alone,
    /// only the codebase set runs.
    #[arg(long, conflicts_with = "fixture")]
    pub codebase: Option<std::path::PathBuf>,
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
            Some(CapAction::Graph(opts)) => return graph(ctx, opts),
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
            Some(CapAction::Bench(opts)) => return bench(ctx, &BenchArgs::from(opts)),
            Some(CapAction::Compare { a, b }) => return compare(ctx, a, b),
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

fn graph(ctx: &Ctx, opts: &GraphOpts) -> Result<ExitCode, ChekovError> {
    let ladder = if opts.ctx.is_empty() {
        vec![32_768, 131_072, 262_144]
    } else {
        opts.ctx.clone()
    };
    let budget = machine::live_gpu_budget(&ctx.config.engine_dir()).ok_or_else(|| {
        ChekovError::SetupIncomplete {
            remaining: "the GPU budget is unknown — run `chekov setup` so the engine \
                        can report it"
                .to_owned(),
        }
    })?;
    let mut f = build_frontier(ctx, &ladder, budget)?;
    if opts.metric == MetricArg::TokS {
        attach_measured_speeds(ctx, &mut f);
    }
    println!("{}", frontier::render_ascii(&f));
    if let Some(requested) = &opts.svg {
        write_svg(ctx, &f, requested.as_deref())?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Under `--metric tok-s`: the stored runs, this machine's identity (so no
/// other machine's run can match), and the engine build now installed (so a
/// measurement from an older build is named, never carried silently).
fn attach_measured_speeds(ctx: &Ctx, f: &mut frontier::Frontier) {
    use crate::core::bench::speeds;
    let cfg = &ctx.config;
    f.metric = frontier::Metric::TokS;
    f.engine_commit = crate::core::engine::recorded_commit(&cfg.logs_dir())
        .or_else(|| crate::core::engine::current_commit(&cfg.engine_dir()));
    let machine_id = machine::machine_id(&machine::probe(&cfg.engine_dir()));
    speeds::attach(f, speeds::load_all(&cfg.eval_dir()), machine_id.as_deref());
}

/// Write the SVG and PRINT its path. Deliberately does not open it: launching
/// a GUI from a CLI is an unrequested side effect, and a printed path composes
/// with whatever the user already uses.
fn write_svg(
    ctx: &Ctx,
    f: &frontier::Frontier,
    requested: Option<&std::path::Path>,
) -> Result<(), ChekovError> {
    let path = requested.map_or_else(
        || {
            ctx.config.reports_dir().join(format!(
                "frontier-{}.svg",
                crate::core::clock::utc_compact_now()
            ))
        },
        std::path::Path::to_path_buf,
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ChekovError::io(format!("creating {}", parent.display()), e))?;
    }
    std::fs::write(&path, frontier::render_svg(f))
        .map_err(|e| ChekovError::io(format!("writing {}", path.display()), e))?;
    println!("svg: {}", path.display());
    Ok(())
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
        let geometry = geometry_for(ctx, &reg, name);
        let q8 = entry.extra_flags.iter().any(|f| f == "q8_0")
            || reg.defaults.flags.iter().any(|f| f == "q8_0");
        let cells = ladder
            .iter()
            .map(|&c| frontier::Cell {
                weights_bytes: weights,
                kv_bytes: kv_for(geometry.as_ref(), c, q8),
                overhead_bytes: Probed::new(Some(3 * 1024 * 1024 * 1024), Provenance::Predicted),
                speed: None,
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
        metric: frontier::Metric::Fit,
        engine_commit: None,
        notes: Vec::new(),
    })
}

/// Real geometry when the first shard is on disk and its header reads.
fn geometry_for(
    ctx: &Ctx,
    reg: &crate::core::registry::Registry,
    name: &str,
) -> Option<crate::core::gguf::Geometry> {
    let eff = reg.effective(name).ok()?;
    let path = crate::core::server::shard_path(&ctx.config, &eff);
    if !path.exists() {
        return None;
    }
    crate::core::gguf::read_geometry(&path).ok()
}

/// KV from the header when it was read; the coarse reserve when it was not —
/// and the cell says which, in its second character.
fn kv_for(
    geometry: Option<&crate::core::gguf::Geometry>,
    ctx: u32,
    q8: bool,
) -> Probed<Option<u64>> {
    let Some(g) = geometry else {
        return Probed::new(Some(kv_reserve(ctx)), Provenance::Predicted);
    };
    crate::core::gguf::kv_bytes(g, ctx, q8).map_or_else(
        || Probed::new(None, Provenance::Predicted),
        |b| Probed::new(Some(b), Provenance::Measured),
    )
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

/// The context a bench needs beyond `Ctx`, resolved and guarded up front.
struct BenchSetup {
    eff: crate::core::registry::Effective,
    pid: i32,
}

/// Which action each requested candidate takes (spec §7.3): reuse the one
/// running server when it IS the single request; otherwise a live server is
/// a refusal — bench never stops a server it did not start.
fn server_use_rule(
    running: Option<&str>,
    requested: &[String],
) -> Result<Vec<crate::core::bench::lifecycle::StepAction>, ChekovError> {
    use crate::core::bench::lifecycle::StepAction;
    match running {
        None => Ok(vec![StepAction::Launch; requested.len()]),
        Some(model) if requested.len() == 1 && requested[0] == model => {
            Ok(vec![StepAction::UseRunning])
        }
        Some(model) => Err(ChekovError::BenchWrongModel {
            running: model.to_owned(),
            resolved: requested.join(","),
        }),
    }
}

/// The requested candidates with their actions, every guard applied.
fn resolve_candidates(
    ctx: &Ctx,
    args: &BenchArgs,
) -> Result<
    Vec<(
        crate::core::registry::Effective,
        crate::core::bench::lifecycle::StepAction,
    )>,
    ChekovError,
> {
    let reg = ctx.registry()?;
    let names: Vec<String> = if args.models.is_empty() {
        vec![reg.active_name()?.to_owned()]
    } else {
        args.models.to_vec()
    };
    if args.resume.is_some() && names.len() > 1 {
        return Err(ChekovError::BenchResumeNeedsOneCandidate);
    }
    let running = match crate::core::server::live_pid(&ctx.config) {
        // A live server whose model is unrecorded cannot be identified —
        // benching through it would attribute numbers to a guess.
        Some(_) => Some(
            crate::core::server::read_run_state(&ctx.config)
                .ok_or(ChekovError::ServerModelUnknown)?,
        ),
        None => None,
    };
    let actions = server_use_rule(running.as_deref(), &names)?;
    names
        .iter()
        .zip(actions)
        .map(|(name, action)| Ok((reg.effective(name)?, action)))
        .collect()
}

/// Wait for `/health` (watching the pid) then assert the loaded `/props`
/// context; returns what the server actually loaded.
fn ensure_ready(
    ctx: &Ctx,
    upstream: &crate::core::proxy::serve::Upstream,
    setup: &BenchSetup,
) -> Result<crate::core::bench::runner::PropsInfo, ChekovError> {
    use crate::core::bench::runner;
    let ready = runner::ReadyTarget {
        base_url: upstream.base_url.clone(),
        pid: setup.pid,
    };
    runner::wait_ready(ctx.http.as_ref(), &ready, (&ctx.config.file.bench).into())?;
    runner::assert_props_ctx(
        &|| crate::core::proxy::serve::get_bearer(upstream, "/props"),
        setup.eff.ctx_size,
    )
}

/// The bench invocation's own inputs, bundled (§4).
struct BenchArgs<'a> {
    fixture: Option<&'a std::path::Path>,
    resume: Option<&'a str>,
    models: &'a [String],
    dry_run: bool,
    yes: bool,
    suite: Option<crate::core::bench::lifecycle::Suite>,
    codebase: Option<&'a std::path::Path>,
}

impl<'a> From<&'a BenchOpts> for BenchArgs<'a> {
    fn from(opts: &'a BenchOpts) -> Self {
        Self {
            fixture: opts.fixture.as_deref(),
            resume: opts.resume.as_deref(),
            models: &opts.models,
            dry_run: opts.dry_run,
            yes: opts.yes,
            suite: effective_suite(opts.suite, opts.codebase.is_some()),
            codebase: opts.codebase.as_deref(),
        }
    }
}

/// `--suite` not passed means `throughput` — unless `--codebase` is given, in
/// which case nothing beyond the codebase set runs.
fn effective_suite(
    passed: Option<crate::core::bench::lifecycle::Suite>,
    codebase: bool,
) -> Option<crate::core::bench::lifecycle::Suite> {
    use crate::core::bench::lifecycle::Suite;
    passed.or(if codebase {
        None
    } else {
        Some(Suite::Throughput)
    })
}

fn codebase_corpus_id(head: &str, set_hash: &str) -> String {
    format!("codebase:{}:{set_hash}", &head[..12.min(head.len())])
}

/// `--codebase`'s gate-through-sample step, or nothing when it wasn't asked
/// for — the one call that touches the user's repository.
///
/// The scratch worktree lives under `<eval>/.scratch/`: a hidden directory,
/// so every enumerator of the eval dir walks past it instead of trying to
/// read a checkout as a bench run.
fn prepare_codebase(
    ctx: &Ctx,
    args: &BenchArgs,
) -> Result<Option<crate::core::bench::codebase::Prepared>, ChekovError> {
    match args.codebase {
        Some(repo) => Ok(Some(crate::core::bench::codebase::prepare(
            repo,
            &ctx.config.eval_dir().join(".scratch"),
            ctx.config.file.bench.codebase_tasks,
        )?)),
        None => Ok(None),
    }
}

/// One candidate's launch inputs, bundled so no callee below grows past 3
/// params (§4).
struct RunInputs<'a> {
    args: &'a BenchArgs<'a>,
    prepared: Option<&'a crate::core::bench::codebase::Prepared>,
}

/// `codebase: {n} tasks from {repo} @ {head[..12]} ({tier census})`, with what
/// the `#[cfg(test)]` cutter took and the shortfall parenthetical appended
/// only when there is something to say.
fn codebase_plan_line(
    prepared: &crate::core::bench::codebase::Prepared,
    repo: &std::path::Path,
) -> String {
    let head12 = &prepared.head[..12.min(prepared.head.len())];
    let census = crate::core::bench::codebase::tier_counts_clause(prepared.counts);
    let elided = if prepared.cfg_test_files == 0 {
        String::new()
    } else {
        format!(", tests elided in {} files", prepared.cfg_test_files)
    };
    let shortfall = if prepared.shortfall.is_empty() {
        String::new()
    } else {
        format!(" ({})", prepared.shortfall.join(", "))
    };
    format!(
        "codebase: {} tasks from {} @ {head12} ({census}){elided}{shortfall}\n",
        prepared.tasks.len(),
        repo.display()
    )
}

/// Six seconds per CROSSING, not per task: a cross-file task is crossed
/// twice, so the estimate is `(in_file + function_body + 2 × cross) × 6`.
fn codebase_estimate_secs(prepared: &crate::core::bench::codebase::Prepared) -> u64 {
    let c = prepared.counts;
    let crossings = c.in_file + c.function_body + 2 * c.cross_file_first;
    u64::try_from(crossings).unwrap_or(0) * 6
}

/// The plan's steps, one per candidate.
fn bench_steps(
    ctx: &Ctx,
    candidates: &[(
        crate::core::registry::Effective,
        crate::core::bench::lifecycle::StepAction,
    )],
) -> Vec<crate::core::bench::lifecycle::BenchStep> {
    candidates
        .iter()
        .map(|(eff, action)| crate::core::bench::lifecycle::BenchStep {
            model: eff.name.clone(),
            action: *action,
            weights_bytes: weights_on_disk(ctx, &eff.entry),
        })
        .collect()
}

/// The wall-clock estimate: the sweep, the agentic crossings, and the
/// codebase set (6s per sampled task).
fn bench_estimate(
    steps: &[crate::core::bench::lifecycle::BenchStep],
    plan: &crate::core::bench::sweep::SweepPlan,
    inputs: &RunInputs,
) -> Result<u64, ChekovError> {
    use crate::core::bench::lifecycle;
    let codebase_secs = inputs.prepared.map_or(0, codebase_estimate_secs);
    Ok(lifecycle::estimate_secs(steps, plan)
        + agentic_estimate_secs(inputs.args.suite)?
        + codebase_secs)
}

/// The dry-run plan: the codebase line (when prepared) ahead of the step
/// table.
fn render_dry_run(
    steps: &[crate::core::bench::lifecycle::BenchStep],
    estimate: u64,
    inputs: &RunInputs,
) -> String {
    use crate::core::bench::lifecycle::render_plan;
    let mut out = String::new();
    if let (Some(p), Some(repo)) = (inputs.prepared, inputs.args.codebase) {
        out.push_str(&codebase_plan_line(p, repo));
    }
    out.push_str(&render_plan(steps, estimate));
    out
}

fn bench(ctx: &Ctx, args: &BenchArgs) -> Result<ExitCode, ChekovError> {
    use crate::core::bench::{lifecycle, sweep};
    // The user's own repository is asked about first: a dirty tree is refused
    // before a single question about servers or models is asked.
    let prepared = prepare_codebase(ctx, args)?;
    let candidates = resolve_candidates(ctx, args)?;
    let inputs = RunInputs {
        args,
        prepared: prepared.as_ref(),
    };
    let plan: sweep::SweepPlan = (&ctx.config.file.bench).into();
    let steps = bench_steps(ctx, &candidates);
    let estimate = bench_estimate(&steps, &plan, &inputs)?;
    if args.dry_run {
        print!("{}", render_dry_run(&steps, estimate, &inputs));
        return Ok(ExitCode::SUCCESS);
    }
    if lifecycle::needs_confirm(&steps) {
        super::confirm(
            &format!(
                "bench {} candidate(s) with launch + teardown, ~{} min estimated",
                steps.len(),
                estimate.div_ceil(60)
            ),
            args.yes,
        )?;
    }
    for candidate in &candidates {
        let dir = run_candidate(ctx, candidate, &inputs)?;
        println!("run: {}", dir.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// One candidate end to end: server up (ours or reused), measure, record —
/// and for launches, teardown with the budget-release check.
fn run_candidate(
    ctx: &Ctx,
    candidate: &(
        crate::core::registry::Effective,
        crate::core::bench::lifecycle::StepAction,
    ),
    inputs: &RunInputs,
) -> Result<std::path::PathBuf, ChekovError> {
    use crate::core::bench::lifecycle::StepAction;
    let (eff, action) = candidate;
    let pid = match action {
        StepAction::UseRunning => {
            crate::core::server::live_pid(&ctx.config).ok_or(ChekovError::ServerNotRunning)?
        }
        StepAction::Launch => launch_candidate(ctx, eff)?,
    };
    let setup = BenchSetup {
        eff: eff.clone(),
        pid,
    };
    let dir = measure_candidate(ctx, &setup, inputs)?;
    if *action == StepAction::Launch {
        teardown_candidate(ctx, pid)?;
    }
    Ok(dir)
}

/// `HeadInputs` from the candidate's props plus the run's own bundle (§4 —
/// keeps `measure_candidate` under the line limit).
fn head_inputs<'a>(
    props: crate::core::bench::runner::PropsInfo,
    plan: &'a crate::core::bench::sweep::SweepPlan,
    inputs: &'a RunInputs,
) -> HeadInputs<'a> {
    HeadInputs {
        props,
        plan,
        fixture: inputs.args.fixture,
        suite: inputs.args.suite,
        codebase: inputs
            .prepared
            .map(|p| (p.head.as_str(), p.set_hash.as_str())),
    }
}

/// Readiness through rendering for one already-up server.
fn measure_candidate(
    ctx: &Ctx,
    setup: &BenchSetup,
    inputs: &RunInputs,
) -> Result<std::path::PathBuf, ChekovError> {
    use crate::core::bench::{store, sweep};
    use crate::core::proxy::serve::Upstream;
    let cfg = &ctx.config;
    let args = inputs.args;
    let upstream = Upstream {
        base_url: cfg.base_url(),
        api_key: cfg.file.server.api_key.clone(),
    };
    let props = ensure_ready(ctx, &upstream, setup)?;
    let plan: sweep::SweepPlan = (&cfg.file.bench).into();
    let head = build_head(ctx, setup, &head_inputs(props, &plan, inputs))?;
    let (mut writer, done) = open_run(ctx, &head, args.resume)?;
    run_suites(
        &mut TaskSink {
            writer: &mut writer,
            done: &done,
        },
        ctx,
        &SuiteInputs {
            plan: &plan,
            upstream: &upstream,
            model: &setup.eff.name,
            fixture: args.fixture,
            suite: args.suite,
            prepared: inputs.prepared,
        },
    )?;
    print!("{}", store::render_run(&store::RunLog::load(writer.dir())?));
    Ok(writer.dir().to_path_buf())
}

/// Preflight, flag hygiene, then a Metal-aware spawn — the same refusal
/// gates as `chekov run`, never a back door around them.
fn launch_candidate(ctx: &Ctx, eff: &crate::core::registry::Effective) -> Result<i32, ChekovError> {
    use crate::core::bench::lifecycle;
    use crate::core::server;
    super::run::preflight(ctx, eff)?;
    let argv = server::launch_args(&ctx.config, eff);
    match lifecycle::server_help(&ctx.config.engine_dir()) {
        Some(help) => {
            if let Some(flag) = lifecycle::unknown_flags(&argv, &help).into_iter().next() {
                return Err(ChekovError::BenchFlagUnknown { flag });
            }
        }
        None => eprintln!(
            "chekov bench: could not capture llama-server --help — flag hygiene unchecked"
        ),
    }
    let pid = server::spawn_daemon_with_env(&ctx.config, eff, &[lifecycle::METAL_RESIDENCY])?;
    server::write_run_state(&ctx.config, &eff.name)?;
    eprintln!("chekov bench: started '{}' (pid {pid})", eff.name);
    Ok(pid)
}

/// Stop what we started, then verify the budget actually came back before
/// the next candidate loads (spec §7.3.8).
fn teardown_candidate(ctx: &Ctx, pid: i32) -> Result<(), ChekovError> {
    use crate::core::bench::lifecycle;
    use crate::core::server::{self, PidFile};
    let cfg = &ctx.config;
    server::stop_pid(pid, std::time::Duration::from_secs(20))?;
    PidFile::new(cfg.pidfile()).remove()?;
    server::clear_run_state(cfg)?;
    let Some(budget) = machine::live_gpu_budget(&cfg.engine_dir()) else {
        eprintln!("chekov bench: budget release UNVERIFIED — the engine probe is unavailable");
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
    eprintln!("chekov bench: budget released ({free} MiB free)");
    Ok(())
}

/// What the suites need beyond the sink and `Ctx` (§4).
struct SuiteInputs<'a> {
    plan: &'a crate::core::bench::sweep::SweepPlan,
    upstream: &'a crate::core::proxy::serve::Upstream,
    model: &'a str,
    fixture: Option<&'a std::path::Path>,
    suite: Option<crate::core::bench::lifecycle::Suite>,
    prepared: Option<&'a crate::core::bench::codebase::Prepared>,
}

fn run_suites(sink: &mut TaskSink, ctx: &Ctx, inputs: &SuiteInputs) -> Result<(), ChekovError> {
    use crate::core::bench::lifecycle::Suite;
    use crate::core::bench::runner;
    use crate::core::proxy::claude::ClaudeFacade;
    let facade = ClaudeFacade::new(inputs.model);
    let wire = runner::ProbeWire {
        http: ctx.http.as_ref(),
        facade: &facade,
        upstream: inputs.upstream,
        pins: runner::SamplingPins {
            seed: ctx.config.file.bench.seed,
        },
    };
    if inputs.suite.is_some_and(Suite::runs_throughput) {
        run_throughput(sink, inputs.plan, &wire)?;
    }
    if inputs.suite.is_some_and(Suite::runs_agentic) {
        run_agentic(sink, &wire)?;
    }
    if let Some(path) = inputs.fixture {
        run_fixture(sink, &wire, path)?;
    }
    if let Some(prepared) = inputs.prepared {
        crate::core::bench::codebase::run::run_codebase(
            &mut crate::core::bench::codebase::run::Sink {
                writer: sink.writer,
                done: sink.done,
            },
            &wire,
            prepared,
        )?;
    }
    Ok(())
}

/// Rough extra seconds for the agentic suites (8s per crossing), from the
/// validated set — a suite that will not run costs nothing. Unconstrained
/// cases cross twice (both doors); the forced pass once.
fn agentic_estimate_secs(
    suite: Option<crate::core::bench::lifecycle::Suite>,
) -> Result<u64, ChekovError> {
    use crate::core::bench::lifecycle::Suite;
    if !suite.is_some_and(Suite::runs_agentic) {
        return Ok(0);
    }
    let set = crate::core::bench::probeset::agentic_v0()?;
    let forced = set
        .tool_emit
        .iter()
        .filter(|c| c.expect == crate::core::bench::probeset::Expect::Call)
        .count();
    let crossings = 2 * set.tool_emit.len() + forced + 2 * set.instruction.len();
    Ok(crossings as u64 * 8)
}

/// The agentic suites (spec §7.2 rows 1, 2, 6): every case appended as its
/// own row, `--resume` skipping recorded ones.
///
/// Every unconstrained case crosses through BOTH doors — buffered, and the
/// streamed one Claude Code actually takes — so a translation defect that
/// exists in only one of them shows up as the same case disagreeing with
/// itself. The grammar-forced pass stays buffered: its axis is the grammar
/// gap, not the transport.
fn run_agentic(
    sink: &mut TaskSink,
    wire: &crate::core::bench::runner::ProbeWire,
) -> Result<(), ChekovError> {
    use crate::core::bench::store::Transport;
    let set = crate::core::bench::probeset::agentic_v0()?;
    // Once the engine has refused a forced grammar, it will refuse every
    // one: record the rest as unavailable rather than firing doomed
    // requests and calling each refusal a model failure.
    let mut refusal = None;
    for transport in [Transport::Buffered, Transport::Streamed] {
        let mut pass = AgenticPass {
            wire,
            transport,
            refusal: refusal.take(),
        };
        for case in &set.tool_emit {
            run_tool_case(sink, &mut pass, case)?;
        }
        for case in &set.instruction {
            run_instruction_case(sink, &pass, case)?;
        }
        refusal = pass.refusal;
    }
    Ok(())
}

/// One door's pass over the agentic set: the wire, which door, and the
/// engine's refusal of forced grammars once it has refused.
struct AgenticPass<'a> {
    wire: &'a crate::core::bench::runner::ProbeWire<'a>,
    transport: crate::core::bench::store::Transport,
    refusal: Option<String>,
}

/// One tool case: the unconstrained crossing through this door, and — for
/// call cases, buffered only — the grammar-forced one. The gap between the
/// two suites is the §7.2 point.
fn run_tool_case(
    sink: &mut TaskSink,
    pass: &mut AgenticPass,
    case: &crate::core::bench::probeset::ToolCase,
) -> Result<(), ChekovError> {
    use crate::core::bench::store::{TaskKey, Transport};
    use crate::core::bench::{grade, probes, probeset, runner};
    let key = TaskKey {
        suite: "tool_emit",
        task_id: &case.id,
        transport: pass.transport,
    };
    if !sink.is_done(&key) {
        let outcome = runner::cross_via(pass.wire, &probes::tool_probe(case), pass.transport).map(
            |artifact| {
                (
                    artifact.timings,
                    grade_row(grade::grade_tool_emit(&artifact.anthropic_body, case)),
                )
            },
        );
        append_probe(sink, key, outcome)?;
    }
    if case.expect != probeset::Expect::Call || pass.transport != Transport::Buffered {
        return Ok(());
    }
    run_forced_case(sink, pass, case)
}

fn run_instruction_case(
    sink: &mut TaskSink,
    pass: &AgenticPass,
    case: &crate::core::bench::probeset::InstructionCase,
) -> Result<(), ChekovError> {
    use crate::core::bench::store::TaskKey;
    use crate::core::bench::{grade, probes, runner};
    let key = TaskKey {
        suite: "instruction",
        task_id: &case.id,
        transport: pass.transport,
    };
    if sink.is_done(&key) {
        return Ok(());
    }
    let outcome = runner::cross_via(pass.wire, &probes::instruction_probe(case), pass.transport)
        .map(|artifact| {
            let (strict, loose) = grade::grade_instruction(&artifact.anthropic_body, case);
            (artifact.timings, instruction_row(strict, &loose))
        });
    append_probe(sink, key, outcome)
}

/// The forced half of one call case.
///
/// An `Err` from the crossing is the engine refusing to constrain the model,
/// not the model answering badly (grading failures arrive as `Ok`) — so it is
/// recorded unavailable, and every later case is too rather than firing more
/// doomed requests.
fn run_forced_case(
    sink: &mut TaskSink,
    forced: &mut AgenticPass,
    case: &crate::core::bench::probeset::ToolCase,
) -> Result<(), ChekovError> {
    use crate::core::bench::store::TaskKey;
    use crate::core::bench::{grade, probes, probeset, runner};
    let forced_id = format!("gg-{}", case.id);
    if sink.is_done(&TaskKey::buffered("grammar_gap", &forced_id)) {
        return Ok(());
    }
    if let Some(reason) = forced.refusal.clone() {
        return append_unavailable(sink, &forced_id, reason);
    }
    let schema = probeset::forced_schema(case);
    match runner::cross_forced(forced.wire, &probes::forced_probe(case), &schema) {
        Ok(artifact) => {
            let verdict = grade_row(grade::grade_forced(&artifact.anthropic_body, case));
            append_probe(
                sink,
                TaskKey::buffered("grammar_gap", &forced_id),
                Ok((artifact.timings, verdict)),
            )
        }
        Err(e) => {
            let reason = e.to_string();
            // Latch ONLY on the engine ANSWERING with a refusal. A chekov-side
            // fault (a body we built wrong, a response we could not read) or a
            // dead socket must not be recorded as an engine limitation — that
            // would turn our own bug, or an outage, into the engine's
            // exoneration and silently skip the remaining cases.
            if matches!(e, ChekovError::UpstreamRefused { .. }) {
                eprintln!(
                    "chekov bench: the engine refused a forced grammar — grammar_gap is \
                     N/A for this run ({reason})"
                );
                forced.refusal = Some(reason.clone());
            } else {
                eprintln!("chekov bench: {} could not be measured ({reason})", case.id);
            }
            append_unavailable(sink, &forced_id, reason)
        }
    }
}

/// Record a task the engine would not let us measure.
fn append_unavailable(
    sink: &mut TaskSink,
    task_id: &str,
    reason: String,
) -> Result<(), ChekovError> {
    use crate::core::bench::store;
    sink.writer.append(store::Task {
        suite: "grammar_gap".into(),
        task_id: task_id.into(),
        measure: crate::core::bench::codebase::run::empty_measure(),
        grade: Some(store::GradeRow::unavailable(reason)),
        transport: store::Transport::Buffered,
        codebase: None,
    })
}

/// Strict is the row's verdict; the loose result rides in the reason so the
/// renderer can compute the chattiness gap.
fn instruction_row(
    strict: crate::core::bench::grade::Grade,
    loose: &crate::core::bench::grade::Grade,
) -> crate::core::bench::store::GradeRow {
    use crate::core::bench::{grade, store};
    let loose_tag = match loose {
        grade::Grade::Pass => "loose:pass".to_owned(),
        grade::Grade::Fail { reason } => format!("loose:fail {reason}"),
    };
    match strict {
        grade::Grade::Pass => store::GradeRow {
            reason: Some(loose_tag),
            ..store::GradeRow::pass()
        },
        grade::Grade::Fail { reason } => store::GradeRow::fail(format!("{reason}; {loose_tag}")),
    }
}

/// Append one graded probe row; a crossing failure records a FAIL with its
/// reason and no invented measurement.
fn append_probe(
    sink: &mut TaskSink,
    key: crate::core::bench::store::TaskKey,
    outcome: Result<
        (
            crate::core::bench::runner::Timings,
            crate::core::bench::store::GradeRow,
        ),
        ChekovError,
    >,
) -> Result<(), ChekovError> {
    use crate::core::bench::store;
    let (measure, verdict) = match outcome {
        Ok((timings, verdict)) => (
            crate::core::bench::codebase::run::probe_measure(&timings),
            verdict,
        ),
        Err(e) => failed_probe(&e),
    };
    sink.writer.append(store::Task {
        suite: key.suite.into(),
        task_id: key.task_id.into(),
        measure,
        grade: Some(verdict),
        transport: key.transport,
        codebase: None,
    })
}

/// Create a fresh run, or reopen one for `--resume` (stamp must match).
fn open_run(
    ctx: &Ctx,
    head: &crate::core::bench::store::RunHead,
    resume: Option<&str>,
) -> Result<(crate::core::bench::store::RunWriter, Vec<Done>), ChekovError> {
    use crate::core::bench::store::RunWriter;
    let eval = ctx.config.eval_dir();
    if let Some(run_id) = resume {
        let (writer, log) = RunWriter::resume(&eval, run_id, head)?;
        let done = log
            .rows
            .iter()
            .map(|r| (r.suite.clone(), r.task_id.clone(), r.transport))
            .collect();
        return Ok((writer, done));
    }
    let run_id = format!("{}-{}", crate::core::clock::utc_compact_now(), head.model);
    Ok((RunWriter::create(&eval, &run_id, head)?, Vec::new()))
}

/// One recorded crossing of a resumed run: suite, case, door.
type Done = (String, String, crate::core::bench::store::Transport);

/// Where task rows land, plus what a resumed run already holds.
struct TaskSink<'a> {
    writer: &'a mut crate::core::bench::store::RunWriter,
    done: &'a [Done],
}

impl TaskSink<'_> {
    fn is_done(&self, key: &crate::core::bench::store::TaskKey) -> bool {
        already_done(self.done, key)
    }
}

/// The `--resume` skip test: the same case through the same door. The other
/// door's crossing is still owed.
fn already_done(done: &[Done], key: &crate::core::bench::store::TaskKey) -> bool {
    done.iter().any(|(suite, task_id, transport)| {
        suite == key.suite && task_id == key.task_id && *transport == key.transport
    })
}

/// Measure each depth and append its row as soon as it completes — a crash
/// or ctrl-C loses at most the depth in flight.
fn run_throughput(
    sink: &mut TaskSink,
    plan: &crate::core::bench::sweep::SweepPlan,
    wire: &crate::core::bench::runner::ProbeWire,
) -> Result<(), ChekovError> {
    use crate::core::bench::{runner, store, sweep};
    for &depth in &plan.depths {
        let task_id = format!("depth-{depth}");
        if sink.is_done(&store::TaskKey::buffered("throughput", &task_id)) {
            eprintln!("chekov: {task_id} already recorded — skipped (--resume)");
            continue;
        }
        let result = sweep::measure_depth(plan, depth, &mut |req| runner::cross(wire, req))?;
        let warmup = result.decode.as_ref().map_or(0, |s| s.warmup_dropped);
        sink.writer.append(store::Task {
            suite: "throughput".into(),
            task_id,
            measure: store::Measure {
                prompt_n: result.prompt_n,
                decode_samples: result.decode_samples,
                prefill_samples: result.prefill_samples,
                warmup_dropped: u32::try_from(warmup).unwrap_or(0),
                cache_n: result.cache_n,
            },
            grade: None,
            transport: store::Transport::Buffered,
            codebase: None,
        })?;
    }
    Ok(())
}

/// Cross and grade every fixture probe. A crossing failure records a FAIL
/// with its reason — a broken exchange must never look like an empty reply.
fn run_fixture(
    sink: &mut TaskSink,
    wire: &crate::core::bench::runner::ProbeWire,
    path: &std::path::Path,
) -> Result<(), ChekovError> {
    use crate::core::bench::store::TaskKey;
    use crate::core::bench::{fixture, grade, probes, runner};
    let loaded = fixture::load(path)?;
    for probe in &loaded.probes {
        if sink.is_done(&TaskKey::buffered("fixture", &probe.id)) {
            eprintln!(
                "chekov: fixture {} already recorded — skipped (--resume)",
                probe.id
            );
            continue;
        }
        let outcome = runner::cross(wire, &probes::fixture_probe(probe)).map(|artifact| {
            (
                artifact.timings,
                grade_row(grade::grade(&artifact.anthropic_body, probe)),
            )
        });
        append_probe(sink, TaskKey::buffered("fixture", &probe.id), outcome)?;
    }
    Ok(())
}

/// A crossing that never completed is UNAVAILABLE, not a failure — for every
/// suite, not just the forced one.
///
/// A server that died mid-run, a context overflow, a refused request: none of
/// them are the model answering badly. Only a reply chekov actually received
/// can be graded, and an ungraded task must not sit in a denominator.
fn failed_probe(
    e: &ChekovError,
) -> (
    crate::core::bench::store::Measure,
    crate::core::bench::store::GradeRow,
) {
    (
        crate::core::bench::codebase::run::empty_measure(),
        crate::core::bench::store::GradeRow::unavailable(e.to_string()),
    )
}

fn grade_row(graded: crate::core::bench::grade::Grade) -> crate::core::bench::store::GradeRow {
    use crate::core::bench::{grade, store};
    match graded {
        grade::Grade::Pass => store::GradeRow::pass(),
        grade::Grade::Fail { reason } => store::GradeRow::fail(reason),
    }
}

/// Everything the stamp is built from beyond `Ctx` and the setup (§4).
struct HeadInputs<'a> {
    props: crate::core::bench::runner::PropsInfo,
    plan: &'a crate::core::bench::sweep::SweepPlan,
    fixture: Option<&'a std::path::Path>,
    suite: Option<crate::core::bench::lifecycle::Suite>,
    /// The codebase run's head and task-set hash, when there is one — drives
    /// `corpus_id` ahead of `suite`/`fixture`.
    codebase: Option<(&'a str, &'a str)>,
}

/// Who measured: the hashed machine identity, its human-readable brand, and
/// the engine commit. Each is required — a stamp cannot pin an unknown.
fn stamp_identity(
    cfg: &crate::core::config::Config,
) -> Result<(String, Option<String>, String), ChekovError> {
    let probed = machine::probe(&cfg.engine_dir());
    let machine_id = machine::machine_id(&probed).ok_or_else(|| ChekovError::SetupIncomplete {
        remaining: "the machine identity is incomplete (model, memory, chip, or GPU \
                    cores unknown) — run `chekov setup` and retry"
            .to_owned(),
    })?;
    let engine = crate::core::engine::recorded_commit(&cfg.logs_dir())
        .or_else(|| crate::core::engine::current_commit(&cfg.engine_dir()))
        .ok_or_else(|| ChekovError::SetupIncomplete {
            remaining: "the engine commit is unknown — run `chekov update --engine` so \
                        the stamp can pin it"
                .to_owned(),
        })?;
    Ok((machine_id, probed.chip, engine))
}

/// The stamp's prompt-set hash and corpus id — split out since both read
/// `inputs.suite` and the seed together (§4, and keeps `build_head` short).
fn head_corpus(
    inputs: &HeadInputs,
    bench_cfg: &crate::core::config::BenchSection,
) -> Result<(String, String), ChekovError> {
    use crate::core::bench::probes;
    let prompt_set_hash = inputs.suite.map_or_else(
        || "codebase-only".to_owned(),
        |suite| probes::suite_prompt_hash(suite, inputs.plan, bench_cfg.seed),
    );
    let corpus = match inputs.codebase {
        Some((head, set_hash)) => codebase_corpus_id(head, set_hash),
        None => corpus_id(inputs.suite, inputs.fixture)?,
    };
    Ok((prompt_set_hash, corpus))
}

fn build_head(
    ctx: &Ctx,
    setup: &BenchSetup,
    inputs: &HeadInputs,
) -> Result<crate::core::bench::store::RunHead, ChekovError> {
    use crate::core::bench::{stamp, store};
    let cfg = &ctx.config;
    let (machine_id, machine_brand, engine) = stamp_identity(cfg)?;
    let launch_args = crate::core::server::launch_args(cfg, &setup.eff);
    let bench_cfg = &cfg.file.bench;
    let flags = stamped_flags(&launch_args);
    let (prompt_set_hash, corpus_id) = head_corpus(inputs, bench_cfg)?;
    let head_stamp = stamp::Stamp {
        machine_id,
        engine_build_commit: engine,
        weights_revision: format!(
            "{}/{}",
            setup.eff.entry.revision, setup.eff.entry.first_shard
        ),
        quant: setup.eff.entry.quant.clone(),
        ctx: inputs.props.n_ctx,
        n_parallel: inputs.props.total_slots,
        kv_unified: flags.kv_unified,
        n_batch: flags.n_batch,
        n_ubatch: flags.n_ubatch,
        type_k: flags.type_k,
        type_v: flags.type_v,
        flash_attn: flags.flash_attn,
        seed: bench_cfg.seed,
        temperature_milli: 0,
        chekov_version: env!("CARGO_PKG_VERSION").to_owned(),
        prompt_set_hash,
        corpus_id,
    };
    // Only a run with a forced pass has a forced reasoning mode to record.
    let forced_reasoning_format = inputs
        .suite
        .is_some_and(crate::core::bench::lifecycle::Suite::runs_agentic)
        .then(|| crate::core::bench::runner::FORCED_REASONING_FORMAT.to_owned());
    Ok(store::RunHead {
        model: setup.eff.name.clone(),
        machine_brand,
        launch_args,
        forced_reasoning_format,
        stamp: head_stamp,
    })
}

/// The stamp's flag-sourced sextet, each spelling covered (§7.4).
struct StampedFlags {
    kv_unified: String,
    n_batch: String,
    n_ubatch: String,
    type_k: String,
    type_v: String,
    flash_attn: String,
}

fn stamped_flags(launch_args: &[String]) -> StampedFlags {
    use crate::core::bench::stamp::flag_value_either as flags;
    StampedFlags {
        kv_unified: flags(launch_args, &["-kvu", "--kv-unified"]),
        n_batch: flags(launch_args, &["-b", "--batch-size"]),
        n_ubatch: flags(launch_args, &["-ub", "--ubatch-size"]),
        type_k: flags(launch_args, &["-ctk", "--cache-type-k"]),
        type_v: flags(launch_args, &["-ctv", "--cache-type-v"]),
        flash_attn: flags(launch_args, &["-fa", "--flash-attn"]),
    }
}

/// The task-set identity, from what the run actually measures — runs over
/// different task sets must never compare as the same set.
fn corpus_id(
    suite: Option<crate::core::bench::lifecycle::Suite>,
    fixture: Option<&std::path::Path>,
) -> Result<String, ChekovError> {
    use crate::core::bench::lifecycle::Suite;
    let agentic = || {
        format!(
            "agentic-v0:{}",
            crate::core::bench::probeset::content_hash()
        )
    };
    let mut id = match suite {
        Some(Suite::Throughput) => "throughput-v1".to_owned(),
        Some(Suite::Agentic) => agentic(),
        Some(Suite::All) => format!("throughput-v1+{}", agentic()),
        // Unreachable from `build_head` (that arm only runs when `codebase`
        // is `None`, and `suite` is `None` only alongside a codebase run) —
        // kept honest rather than `unreachable!()`.
        None => "codebase-only".to_owned(),
    };
    if let Some(path) = fixture {
        let text = std::fs::read_to_string(path).map_err(|e| ChekovError::FixtureInvalid {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
        let digest = crate::core::hash::sha256_hex(text.as_bytes());
        id = format!("{id}+fixture:{}", &digest[..12]);
    }
    Ok(id)
}

/// A run argument is a directory path, or a run id under `eval/`.
fn resolve_run(ctx: &Ctx, arg: &std::path::Path) -> std::path::PathBuf {
    if arg.is_dir() {
        arg.to_path_buf()
    } else {
        ctx.config.eval_dir().join(arg)
    }
}

fn compare(ctx: &Ctx, a: &std::path::Path, b: &std::path::Path) -> Result<ExitCode, ChekovError> {
    use crate::core::bench::{compare as bench_compare, store};
    let run_a = store::RunLog::load(&resolve_run(ctx, a))?;
    let run_b = store::RunLog::load(&resolve_run(ctx, b))?;
    let rows = bench_compare::compare_runs(
        &run_a,
        &run_b,
        f64::from(ctx.config.file.bench.significance_pct),
    )?;
    print!(
        "{}",
        bench_compare::render_comparison(
            &bench_compare::RunPair {
                a: &run_a,
                b: &run_b
            },
            &rows
        )
    );
    Ok(ExitCode::SUCCESS)
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
    fn bench_and_compare_parse() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from([
            "chekov",
            "capability",
            "bench",
            "--fixture",
            "probes.toml",
        ])
        .expect("bench parses");
        match cli.cmd {
            crate::cli::Cmd::Capability(cap) => match cap.action {
                Some(super::CapAction::Bench(opts)) => {
                    assert_eq!(opts.resume, None);
                    assert!(opts.models.is_empty() && !opts.dry_run && !opts.yes);
                    assert_eq!(
                        opts.fixture.as_deref(),
                        Some(std::path::Path::new("probes.toml"))
                    );
                }
                other => panic!("expected Bench, got {other:?}"),
            },
            _ => panic!("expected capability"),
        }
        let cli = crate::cli::Cli::try_parse_from([
            "chekov",
            "capability",
            "compare",
            "a.json",
            "b.json",
        ])
        .expect("compare parses");
        match cli.cmd {
            crate::cli::Cmd::Capability(cap) => {
                assert!(matches!(cap.action, Some(super::CapAction::Compare { .. })));
            }
            _ => panic!("expected capability"),
        }
    }

    #[test]
    fn models_flag_parses_a_comma_list() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from([
            "chekov",
            "capability",
            "bench",
            "--models",
            "a,b",
            "--dry-run",
        ])
        .expect("parses");
        match cli.cmd {
            crate::cli::Cmd::Capability(cap) => match cap.action {
                Some(super::CapAction::Bench(opts)) => {
                    assert_eq!(opts.models, vec!["a".to_owned(), "b".to_owned()]);
                    assert!(opts.dry_run);
                }
                other => panic!("expected Bench, got {other:?}"),
            },
            _ => panic!("expected capability"),
        }
    }

    #[test]
    fn suite_flag_parses_and_defaults_to_throughput() {
        use crate::core::bench::lifecycle::Suite;
        use clap::Parser;
        let cli =
            crate::cli::Cli::try_parse_from(["chekov", "capability", "bench"]).expect("parses");
        match cli.cmd {
            crate::cli::Cmd::Capability(cap) => match cap.action {
                Some(super::CapAction::Bench(opts)) => {
                    assert_eq!(opts.suite, None, "--suite not passed");
                }
                other => panic!("expected Bench, got {other:?}"),
            },
            _ => panic!("expected capability"),
        }
        let cli = crate::cli::Cli::try_parse_from([
            "chekov",
            "capability",
            "bench",
            "--suite",
            "agentic",
        ])
        .expect("parses");
        match cli.cmd {
            crate::cli::Cmd::Capability(cap) => match cap.action {
                Some(super::CapAction::Bench(opts)) => {
                    assert_eq!(opts.suite, Some(Suite::Agentic));
                }
                other => panic!("expected Bench, got {other:?}"),
            },
            _ => panic!("expected capability"),
        }
    }

    #[test]
    fn codebase_and_fixture_conflict_and_suite_is_optional() {
        use clap::Parser;
        assert!(
            crate::cli::Cli::try_parse_from([
                "chekov",
                "capability",
                "bench",
                "--codebase",
                ".",
                "--fixture",
                "f.toml"
            ])
            .is_err(),
            "mutually exclusive"
        );
        let cli =
            crate::cli::Cli::try_parse_from(["chekov", "capability", "bench", "--codebase", "."])
                .expect("parses");
        match cli.cmd {
            crate::cli::Cmd::Capability(cap) => match cap.action {
                Some(super::CapAction::Bench(opts)) => {
                    assert_eq!(opts.codebase.as_deref(), Some(std::path::Path::new(".")));
                    assert_eq!(opts.suite, None, "--suite not passed");
                }
                other => panic!("expected Bench, got {other:?}"),
            },
            _ => panic!("expected capability"),
        }
    }

    #[test]
    fn the_effective_suite_is_throughput_by_default_and_nothing_extra_with_codebase_alone() {
        use crate::core::bench::lifecycle::Suite;
        assert_eq!(super::effective_suite(None, false), Some(Suite::Throughput));
        assert_eq!(
            super::effective_suite(None, true),
            None,
            "codebase alone runs only codebase"
        );
        assert_eq!(
            super::effective_suite(Some(Suite::All), true),
            Some(Suite::All)
        );
    }

    use crate::core::bench::codebase::ladder::Symbols;
    use crate::core::bench::codebase::{CodebaseTask, Counts, Excluded, Prepared, TaskTier};

    /// A task shaped only enough to be counted — the plan line reads
    /// `tasks.len()` and `counts`, nothing else.
    fn plan_task(line: usize) -> CodebaseTask {
        CodebaseTask {
            id: format!("in_file-abc123-L{line}"),
            tier: TaskTier::InFile,
            file: "src/a.rs".into(),
            line,
            gold: "let a = 1;".into(),
            prefix: "fn f() {\n".into(),
            suffix: "\n}\n".into(),
            excluded: Excluded {
                doc_comment: 0,
                cross_file: "n/a: same-file".into(),
                cfg_test_lines: 0,
                cross_file_withheld: 0,
            },
            also_first_uses: vec![],
            extra: None,
            extra_text: String::new(),
        }
    }

    fn prepared_counts(in_file: usize, function_body: usize, cross: usize) -> Prepared {
        Prepared {
            head: "4818813deeaa11112222333344445555666677".into(),
            set_hash: "abcdef123456".into(),
            tasks: (0..in_file + function_body + cross)
                .map(plan_task)
                .collect(),
            shortfall: vec![],
            symbols: Symbols::default(),
            cfg_test_lines: 0,
            cfg_test_files: 0,
            counts: Counts {
                in_file,
                function_body,
                cross_file_first: cross,
            },
        }
    }

    #[test]
    fn the_plan_line_names_every_tier_and_the_second_arm() {
        let line =
            super::codebase_plan_line(&prepared_counts(12, 6, 6), std::path::Path::new("/r"));
        assert_eq!(
            line,
            "codebase: 24 tasks from /r @ 4818813deeaa (12 in_file, 6 function_body, \
             6 cross_file_first × 2 arms)\n",
            "{line}"
        );
        let none =
            super::codebase_plan_line(&prepared_counts(12, 12, 0), std::path::Path::new("/r"));
        assert!(
            none.contains("(12 in_file, 12 function_body, 0 cross_file_first)"),
            "no second arm to announce: {none}"
        );
    }

    #[test]
    fn the_estimate_counts_a_cross_file_task_twice() {
        assert_eq!(
            super::codebase_estimate_secs(&prepared_counts(12, 6, 6)),
            180
        );
        assert_eq!(
            super::codebase_estimate_secs(&prepared_counts(12, 12, 0)),
            144
        );
    }

    #[test]
    fn the_codebase_corpus_id_pins_head_and_the_task_set() {
        let id = super::codebase_corpus_id("0123456789abcdef0123", "fedcba987654");
        assert_eq!(id, "codebase:0123456789ab:fedcba987654");
    }

    #[test]
    fn the_suite_hash_keeps_throughput_stable_and_separates_the_rest() {
        use crate::core::bench::lifecycle::Suite;
        use crate::core::bench::{probes, sweep};
        let plan = sweep::SweepPlan {
            depths: vec![1024],
            repetitions: 5,
            max_tokens: 128,
        };
        assert_eq!(
            probes::suite_prompt_hash(Suite::Throughput, &plan, 42),
            probes::prompt_set_hash(&plan, 42),
            "runs recorded before --suite existed stay comparable"
        );
        let agentic = probes::suite_prompt_hash(Suite::Agentic, &plan, 42);
        let all = probes::suite_prompt_hash(Suite::All, &plan, 42);
        assert_ne!(agentic, all);
        assert_ne!(agentic, probes::prompt_set_hash(&plan, 42));
    }

    #[test]
    fn the_server_use_rule_reuses_refuses_and_launches() {
        use crate::core::bench::lifecycle::StepAction;
        let one = ["m1".to_owned()];
        let two = ["m1".to_owned(), "m2".to_owned()];
        // No server: launch everything.
        assert_eq!(
            super::server_use_rule(None, &two).expect("launch all"),
            vec![StepAction::Launch, StepAction::Launch]
        );
        // The single request IS the running model: reuse, leave it up.
        assert_eq!(
            super::server_use_rule(Some("m1"), &one).expect("reuse"),
            vec![StepAction::UseRunning]
        );
        // A live server bench did not start is never stopped for a sweep.
        assert!(super::server_use_rule(Some("m1"), &two).is_err());
        assert!(super::server_use_rule(Some("other"), &one).is_err());
    }

    #[test]
    fn metric_parses_tok_s_and_defaults_to_fit() {
        use clap::Parser;
        let metric_of = |args: &[&str]| {
            let cli = crate::cli::Cli::try_parse_from(args).expect("parses");
            match cli.cmd {
                crate::cli::Cmd::Capability(cap) => match cap.action {
                    Some(super::CapAction::Graph(opts)) => opts.metric,
                    other => panic!("expected Graph, got {other:?}"),
                },
                _ => panic!("expected capability"),
            }
        };
        assert_eq!(
            metric_of(&["chekov", "capability", "graph"]),
            super::MetricArg::Fit
        );
        assert_eq!(
            metric_of(&["chekov", "capability", "graph", "--metric", "tok-s"]),
            super::MetricArg::TokS
        );
        assert!(
            crate::cli::Cli::try_parse_from(["chekov", "capability", "graph", "--metric", "speed"])
                .is_err(),
            "only the spec's two values parse"
        );
    }

    #[test]
    fn a_resumed_run_still_owes_the_other_door() {
        use crate::core::bench::store::{TaskKey, Transport};
        let done = vec![(
            "tool_emit".to_owned(),
            "te-001".to_owned(),
            Transport::Buffered,
        )];
        assert!(super::already_done(
            &done,
            &TaskKey::buffered("tool_emit", "te-001")
        ));
        assert!(!super::already_done(
            &done,
            &TaskKey::streamed("tool_emit", "te-001")
        ));
        assert!(!super::already_done(
            &done,
            &TaskKey::buffered("tool_emit", "te-002")
        ));
    }

    #[test]
    fn the_agentic_estimate_counts_both_doors() {
        use crate::core::bench::lifecycle::Suite;
        use crate::core::bench::probeset::{Expect, agentic_v0};
        let set = agentic_v0().expect("the compiled-in set is valid");
        let forced = set
            .tool_emit
            .iter()
            .filter(|c| c.expect == Expect::Call)
            .count();
        // Every unconstrained case crosses twice (buffered and streamed); the
        // forced pass crosses once.
        let cases = 2 * set.tool_emit.len() + forced + 2 * set.instruction.len();
        assert_eq!(
            super::agentic_estimate_secs(Some(Suite::Agentic)).expect("estimate"),
            cases as u64 * 8
        );
        assert_eq!(
            super::agentic_estimate_secs(Some(Suite::Throughput)).expect("estimate"),
            0
        );
    }

    #[test]
    fn svg_parses_absent_bare_and_with_a_path() {
        use clap::Parser;
        let svg_of = |args: &[&str]| {
            let cli = crate::cli::Cli::try_parse_from(args).expect("parses");
            match cli.cmd {
                crate::cli::Cmd::Capability(cap) => match cap.action {
                    Some(super::CapAction::Graph(opts)) => opts.svg,
                    other => panic!("expected Graph, got {other:?}"),
                },
                _ => panic!("expected capability"),
            }
        };
        assert_eq!(svg_of(&["chekov", "capability", "graph"]), None, "absent");
        assert_eq!(
            svg_of(&["chekov", "capability", "graph", "--svg"]),
            Some(None),
            "bare — the default report path is chosen at write time"
        );
        assert_eq!(
            svg_of(&["chekov", "capability", "graph", "--svg", "/tmp/f.svg"]),
            Some(Some(std::path::PathBuf::from("/tmp/f.svg"))),
        );
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
