//! `chekov capability` — doctor's twin for the machine rather than the server.
//!
//! Reports what this Mac is and what it can hold. Every number carries where
//! it came from, because the arithmetic rung is measurably 30.7 GiB low on a
//! 256 GiB M3 Ultra and a bare figure gives the reader no way to know that.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::core::bench::candidate::{self, Candidate};
use crate::core::bench::runtime::{self, RuntimeSpec};
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
        /// Permit the runtime allow-list (spec §7) to differ — a loud
        /// banner prints before any other output.
        #[arg(long)]
        cross_runtime: bool,
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
    /// Run the repository's own build for tiers 6-7 (compile gate, covering
    /// test). The SINGLE gate on every path that executes repository code:
    /// `cargo check` and `cargo test` run its `build.rs`, its proc-macros and
    /// its tests — the same trust as building it yourself. Bounded to a
    /// detached worktree, offline after one fetch, a scratch target directory
    /// and wall-clock timeouts; not a sandbox.
    #[arg(long)]
    pub allow_exec: bool,
    /// A registered `role = "judge"` model, loaded in its own phase after
    /// every candidate is down, answering one position-swapped binary
    /// question per `function_body` crossing (spec C). Refused before any
    /// launch when it lacks the role, shares a family with a candidate, or
    /// would have to stop a server bench did not start.
    #[arg(long, requires = "codebase")]
    pub judge: Option<String>,
    /// A foreign runtime serving the subject (`<name>@<version>`, e.g.
    /// `mtplx@0.4.1`). Declared, never probed: chekov measures a server YOU
    /// started, and refuses to launch one (spec 2026-08-31 §2).
    #[arg(long)]
    pub runtime: Option<String>,
    /// Base URL of the foreign server (default: the configured endpoint).
    #[arg(long, requires = "runtime")]
    pub upstream: Option<String>,
    /// Which served id names the subject on the request wire (default: the
    /// single id `/v1/models` lists; required when it lists zero or several).
    /// chekov addresses a foreign server by what it serves, never by its own
    /// registry name — mlx-lm routes on the `model` field and 404s trying to
    /// download chekov's name (finding (a)).
    #[arg(long, requires = "runtime")]
    pub served_model: Option<String>,
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
            Some(CapAction::Bench(opts)) => return bench(ctx, &bench_args(opts)?),
            Some(CapAction::Compare {
                a,
                b,
                cross_runtime,
            }) => {
                return compare(ctx, &compare_args(a, b, *cross_runtime));
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
        let q8 = crate::core::footprint::wants_q8(&entry.extra_flags)
            || crate::core::footprint::wants_q8(&reg.defaults.flags);
        let cells = ladder
            .iter()
            .map(|&c| frontier::Cell {
                weights_bytes: weights,
                kv_bytes: kv_for(geometry.as_ref(), c, q8),
                overhead_bytes: Probed::new(
                    Some(crate::core::footprint::OVERHEAD_BYTES),
                    Provenance::Predicted,
                ),
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

/// Bytes actually on disk for a model directory — `footprint`'s sum, so the
/// gate, `recommend` and `graph` size a model the same way.
fn weights_on_disk(ctx: &Ctx, entry: &crate::core::registry::ModelEntry) -> Option<u64> {
    crate::core::footprint::weights_on_disk(&ctx.config.root, entry)
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
    // The nextn layers are a native multi-token-prediction draft head baked
    // into the weights. llama.cpp loads past them — `kv_layers` subtracts
    // them for exactly that reason — so the note says what the number IS and
    // that the engine leaves it idle, rather than printing a bare count only
    // the KV arithmetic understands.
    let nextn = g.nextn_predict_layers.unwrap_or(0);
    let _ = writeln!(
        out,
        "  nextn_predict_layers    {nextn}{}",
        if nextn > 0 {
            "   (a native MTP draft head; this engine decodes without it)"
        } else {
            ""
        }
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
    let eff = reg.effective(name).ok();
    let geometry = eff
        .as_ref()
        .map(|eff| crate::core::server::shard_path(&ctx.config, eff))
        .filter(|p| p.exists())
        .and_then(|p| crate::core::gguf::read_geometry(&p).ok());
    // The flags the model is actually launched with — `run` reads the same
    // ones, so the two cannot disagree about the KV cache's type.
    let q8 = eff
        .as_ref()
        .is_some_and(|eff| crate::core::footprint::wants_q8(&eff.flags));
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
        total_bytes: weights
            .zip(kv)
            .map(|(w, k)| crate::core::footprint::sized(w, k)),
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

/// A declared foreign runtime benches exactly ONE already-served subject.
/// chekov holds no pid and no run-state file for a server it did not start,
/// so there is nothing to consult and nothing it could launch (spec §2).
fn foreign_actions(
    spec: &RuntimeSpec,
    requested: &[String],
) -> Result<Vec<crate::core::bench::lifecycle::StepAction>, ChekovError> {
    use crate::core::bench::lifecycle::StepAction;
    if requested.len() != 1 {
        return Err(ChekovError::RuntimeNeedsRunningServer {
            runtime: spec.stored(),
        });
    }
    Ok(vec![StepAction::UseRunning])
}

/// Which action each requested candidate takes: the foreign path is
/// `UseRunning`-only and never asks chekov's own server state, the llama.cpp
/// path asks as it always has (§7.3).
fn candidate_actions(
    ctx: &Ctx,
    args: &BenchArgs,
    names: &[String],
) -> Result<Vec<crate::core::bench::lifecycle::StepAction>, ChekovError> {
    if let Some(spec) = args.runtime.as_ref() {
        return foreign_actions(spec, names);
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
    server_use_rule(running.as_deref(), names)
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
    let actions = candidate_actions(ctx, args, &names)?;
    names
        .iter()
        .zip(actions)
        .map(|(name, action)| Ok((reg.effective(name)?, action)))
        .collect()
}

/// A judge is named on purpose: the role is the naming, and its absence is a
/// refusal rather than a silently-ordinary model.
fn role_check(eff: &crate::core::registry::Effective, name: &str) -> Result<(), ChekovError> {
    if eff.entry.role == Some(crate::core::registry::ModelRole::Judge) {
        return Ok(());
    }
    Err(ChekovError::JudgeNoRole {
        name: name.to_owned(),
    })
}

/// The judge phase loads after every candidate is down — which it cannot do
/// while a server bench did not start holds the budget.
fn server_check(
    candidates: &[(
        crate::core::registry::Effective,
        crate::core::bench::lifecycle::StepAction,
    )],
) -> Result<(), ChekovError> {
    use crate::core::bench::lifecycle::StepAction;
    if candidates
        .iter()
        .any(|(_, action)| *action == StepAction::UseRunning)
    {
        return Err(ChekovError::JudgeNeedsTheServer);
    }
    Ok(())
}

/// `general.architecture` from a model's first shard — the family check
/// cannot proceed without it, so a missing shard is the existing preflight
/// refusal, not a guess.
fn arch_of(ctx: &Ctx, eff: &crate::core::registry::Effective) -> Result<String, ChekovError> {
    let path = crate::core::server::shard_path(&ctx.config, eff);
    Ok(crate::core::gguf::read_geometry(&path)?.arch)
}

/// `--judge`, resolved before any launch: role, no foreign server to stop,
/// no shared family — each a refusal that names its remedy.
fn resolve_judge(
    ctx: &Ctx,
    args: &BenchArgs,
    candidates: &[(
        crate::core::registry::Effective,
        crate::core::bench::lifecycle::StepAction,
    )],
) -> Result<Option<crate::core::bench::judge::JudgePlan>, ChekovError> {
    use crate::core::bench::judge;
    let Some(name) = args.judge else {
        return Ok(None);
    };
    let eff = ctx.registry()?.effective(name)?;
    role_check(&eff, name)?;
    server_check(candidates)?;
    let arch = arch_of(ctx, &eff)?;
    let archs: Vec<(String, String)> = candidates
        .iter()
        .map(|(c, _)| Ok((c.name.clone(), arch_of(ctx, c)?)))
        .collect::<Result<_, ChekovError>>()?;
    if let Some(conflict) = judge::family_conflict((name, &arch), &archs) {
        return Err(conflict);
    }
    let bench_cfg = &ctx.config.file.bench;
    Ok(Some(judge::JudgePlan {
        judge: eff,
        arch,
        rubric_hash: judge::rubric_hash(),
        max_tokens: bench_cfg.judge_max_tokens,
        min_consistency_pct: bench_cfg.judge_min_consistency_pct,
        reasoning_effort: bench_cfg.judge_reasoning_effort,
    }))
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
    allow_exec: bool,
    judge: Option<&'a str>,
    /// The declared foreign runtime, when there is one — the whole foreign
    /// path hangs off this being `Some` (spec §2).
    runtime: Option<RuntimeSpec>,
    /// `--upstream`, overriding the configured endpoint for this run only.
    upstream: Option<&'a str>,
    /// `--served-model`, naming which of the foreign server's served ids is
    /// the subject (spec finding (a)).
    served_model: Option<&'a str>,
}

/// The parsed invocation. Fallible where `From` could not be: a malformed
/// `--runtime` is refused HERE, before the repository, the registry or any
/// server is asked about.
fn bench_args(opts: &BenchOpts) -> Result<BenchArgs<'_>, ChekovError> {
    Ok(BenchArgs {
        fixture: opts.fixture.as_deref(),
        resume: opts.resume.as_deref(),
        models: &opts.models,
        dry_run: opts.dry_run,
        yes: opts.yes,
        suite: effective_suite(opts.suite, opts.codebase.is_some()),
        codebase: opts.codebase.as_deref(),
        allow_exec: opts.allow_exec,
        judge: opts.judge.as_deref(),
        runtime: opts
            .runtime
            .as_deref()
            .map(RuntimeSpec::parse)
            .transpose()?,
        upstream: opts.upstream.as_deref(),
        served_model: opts.served_model.as_deref(),
    })
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
            &crate::core::bench::codebase::PrepareInputs {
                scratch_root: &ctx.config.eval_dir().join(".scratch"),
                tasks: ctx.config.file.bench.codebase_tasks,
                allow_exec: args.allow_exec,
            },
        )?)),
        None => Ok(None),
    }
}

/// One candidate's launch inputs, bundled so no callee below grows past 3
/// params (§4).
struct RunInputs<'a> {
    args: &'a BenchArgs<'a>,
    prepared: Option<&'a crate::core::bench::codebase::Prepared>,
    /// The judge phase's plan, resolved before any launch — `None` without
    /// `--judge`.
    judge: Option<&'a crate::core::bench::judge::JudgePlan>,
    /// How many candidates the judge will be asked about: every crossing is
    /// judged once per candidate run.
    candidates: usize,
}

/// `codebase: {n} tasks from {repo} @ {head[..12]} ({tier census})`, with what
/// the `#[cfg(test)]` cutter took and the shortfall parenthetical appended
/// only when there is something to say.
fn codebase_plan_line(
    prepared: &crate::core::bench::codebase::Prepared,
    repo: &std::path::Path,
    allow_exec: bool,
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
    let exec = if allow_exec {
        " + exec: cold check unmeasured, then ~6 s per crossing"
    } else {
        ""
    };
    format!(
        "codebase: {} tasks from {} @ {head12} ({census}){elided}{shortfall}{exec}\n",
        prepared.tasks.len(),
        repo.display()
    )
}

/// Six seconds per CROSSING, not per task: a cross-file task is crossed
/// twice, so the estimate is `(in_file + function_body + 2 × cross) × 6` —
/// doubled under `--allow-exec`, where each crossing also pays for a check.
///
/// Six seconds for a warm incremental check is a guess, and the run replaces
/// it with the measured pair as soon as it has two of them.
fn codebase_estimate_secs(
    prepared: &crate::core::bench::codebase::Prepared,
    allow_exec: bool,
) -> u64 {
    let c = prepared.counts;
    let crossings = c.in_file + c.function_body + 2 * c.cross_file_first;
    let per_crossing = if allow_exec { 12 } else { 6 };
    u64::try_from(crossings).unwrap_or(0) * per_crossing
}

/// The plan's steps, one per candidate — and the judge's own step last, the
/// one load that happens after every candidate is down.
fn bench_steps(
    ctx: &Ctx,
    candidates: &[(
        crate::core::registry::Effective,
        crate::core::bench::lifecycle::StepAction,
    )],
    judge: Option<&crate::core::bench::judge::JudgePlan>,
) -> Vec<crate::core::bench::lifecycle::BenchStep> {
    use crate::core::bench::lifecycle::{BenchStep, StepAction};
    let mut steps: Vec<BenchStep> = candidates
        .iter()
        .map(|(eff, action)| BenchStep {
            model: eff.name.clone(),
            action: *action,
            weights_bytes: weights_on_disk(ctx, &eff.entry),
        })
        .collect();
    if let Some(plan) = judge {
        steps.push(BenchStep {
            model: plan.judge.name.clone(),
            action: StepAction::Judge,
            weights_bytes: weights_on_disk(ctx, &plan.judge.entry),
        });
    }
    steps
}

/// Every `function_body` crossing of every candidate run — what the judge
/// phase will be asked, and zero when `--judge` was not given.
fn judge_crossings(inputs: &RunInputs) -> u64 {
    let (Some(_), Some(prepared)) = (inputs.judge, inputs.prepared) else {
        return 0;
    };
    u64::try_from(prepared.counts.function_body).unwrap_or(0)
        * u64::try_from(inputs.candidates).unwrap_or(0)
}

/// The wall-clock estimate: the sweep, the agentic crossings, and the
/// codebase set (6s per crossing — a `cross_file_first` task is two).
fn bench_estimate(
    steps: &[crate::core::bench::lifecycle::BenchStep],
    plan: &crate::core::bench::sweep::SweepPlan,
    inputs: &RunInputs,
) -> Result<u64, ChekovError> {
    use crate::core::bench::lifecycle;
    let codebase_secs = inputs
        .prepared
        .map_or(0, |p| codebase_estimate_secs(p, inputs.args.allow_exec));
    Ok(lifecycle::estimate_secs(steps, plan)
        + agentic_estimate_secs(inputs.args.suite)?
        + codebase_secs
        + lifecycle::judge_estimate_secs(judge_crossings(inputs)))
}

/// The dry-run plan: the codebase line (when prepared) and the judge's own
/// line ahead of the step table.
fn render_dry_run(
    steps: &[crate::core::bench::lifecycle::BenchStep],
    estimate: u64,
    inputs: &RunInputs,
) -> String {
    use crate::core::bench::lifecycle::render_plan;
    let mut out = String::new();
    if let (Some(p), Some(repo)) = (inputs.prepared, inputs.args.codebase) {
        out.push_str(&codebase_plan_line(p, repo, inputs.args.allow_exec));
    }
    if inputs.judge.is_some() {
        out.push_str(&judge_plan_line(judge_crossings(inputs)));
    }
    out.push_str(&render_plan(steps, estimate));
    out
}

/// What the judge phase adds to the plan: two orders a crossing, at the
/// measured seconds a verdict.
fn judge_plan_line(crossings: u64) -> String {
    use crate::core::bench::lifecycle::JUDGE_SECS_PER_VERDICT;
    format!("+ judge: 2 orders × {crossings} verdicts, ~{JUDGE_SECS_PER_VERDICT} s each\n")
}

fn bench(ctx: &Ctx, args: &BenchArgs) -> Result<ExitCode, ChekovError> {
    use crate::core::bench::sweep;
    // The user's own repository is asked about first: a dirty tree is refused
    // before a single question about servers or models is asked.
    let prepared = prepare_codebase(ctx, args)?;
    let candidates = resolve_candidates(ctx, args)?;
    let judge = resolve_judge(ctx, args, &candidates)?;
    let inputs = RunInputs {
        args,
        prepared: prepared.as_ref(),
        judge: judge.as_ref(),
        candidates: candidates.len(),
    };
    let plan: sweep::SweepPlan = (&ctx.config.file.bench).into();
    let steps = bench_steps(ctx, &candidates, judge.as_ref());
    let estimate = bench_estimate(&steps, &plan, &inputs)?;
    if args.dry_run {
        print!("{}", render_dry_run(&steps, estimate, &inputs));
        return finish_codebase(prepared).map(|()| ExitCode::SUCCESS);
    }
    confirm_launches(&steps, estimate, args.yes)?;
    let outcome = run_candidates(ctx, &candidates, &inputs)
        .and_then(|dirs| judge_phase(ctx, &dirs, judge.as_ref()));
    finish_codebase(prepared)?;
    outcome.map(|()| ExitCode::SUCCESS)
}

/// The gate any launch step asks for — a model load and a teardown are real
/// side effects, and a pure reuse run stays gate-free.
fn confirm_launches(
    steps: &[crate::core::bench::lifecycle::BenchStep],
    estimate: u64,
    yes: bool,
) -> Result<(), ChekovError> {
    if !crate::core::bench::lifecycle::needs_confirm(steps) {
        return Ok(());
    }
    super::confirm(&confirm_text(steps, estimate), yes)
}

/// What the gate asks about. The judge is a step but never a candidate: it is
/// counted apart and named, so agreeing to "3 candidate(s)" cannot mean four
/// loads.
fn confirm_text(steps: &[crate::core::bench::lifecycle::BenchStep], estimate: u64) -> String {
    use crate::core::bench::lifecycle::StepAction;
    let judge = steps
        .iter()
        .find(|s| s.action == StepAction::Judge)
        .map_or_else(String::new, |s| format!(" plus judge '{}'", s.model));
    let candidates = steps
        .iter()
        .filter(|s| s.action != StepAction::Judge)
        .count();
    format!(
        "bench {candidates} candidate(s){judge} with launch + teardown, ~{} min estimated",
        estimate.div_ceil(60)
    )
}

/// The judge phase, or nothing to do — `--judge` was not given, or no run
/// holds a crossing the judge could be asked about (spec C §7). A judge is
/// never loaded to record nothing.
fn judge_phase(
    ctx: &Ctx,
    runs: &[std::path::PathBuf],
    judge: Option<&crate::core::bench::judge::JudgePlan>,
) -> Result<(), ChekovError> {
    let Some(plan) = judge else {
        return Ok(());
    };
    if eligible_crossings(runs)? == 0 {
        eprintln!("chekov bench: judge skipped — nothing eligible");
        return Ok(());
    }
    run_judge_phase(ctx, runs, plan)
}

/// How many stored crossings the judge would be asked about, across every run
/// — the same eligibility AND the same resume skip the phase itself applies,
/// read before it launches. A fully judged run resumed with `--judge` owes
/// nothing, so nothing is loaded to record nothing.
fn eligible_crossings(runs: &[std::path::PathBuf]) -> Result<usize, ChekovError> {
    use crate::core::bench::judge::eligibility;
    use crate::core::bench::store::{self, JUDGE_SUITE, RunLog, TaskKey};
    let mut total = 0;
    for dir in runs {
        let log = RunLog::load(dir)?;
        total += log
            .rows
            .iter()
            .filter(|r| r.suite == "codebase" && !store::is_unavailable(r))
            .filter(|r| !log.is_done(&TaskKey::buffered(JUDGE_SUITE, &r.task_id)))
            .filter_map(|r| r.codebase.as_ref())
            .filter(|row| eligibility(row).is_some())
            .count();
    }
    Ok(total)
}

/// Every candidate, each one's run directory printed as it lands.
fn run_candidates(
    ctx: &Ctx,
    candidates: &[(
        crate::core::registry::Effective,
        crate::core::bench::lifecycle::StepAction,
    )],
    inputs: &RunInputs,
) -> Result<Vec<std::path::PathBuf>, ChekovError> {
    let mut dirs = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let dir = run_candidate(ctx, candidate, inputs)?;
        println!("run: {}", dir.display());
        dirs.push(dir);
    }
    Ok(dirs)
}

/// The worktree and the scratch target directory, removed with the run.
///
/// Explicit rather than left to `Worktree::drop`, so a cleanup that fails is
/// reported. Without `--allow-exec` there is nothing here to remove: `prepare`
/// took the worktree away before it returned.
fn finish_codebase(
    prepared: Option<crate::core::bench::codebase::Prepared>,
) -> Result<(), ChekovError> {
    match prepared {
        Some(p) => p.exec.finish(),
        None => Ok(()),
    }
}

/// The pid this candidate is measured against. A foreign server has none:
/// chekov did not start it, holds no pidfile for it, and will not watch or
/// stop it (spec §2, §4).
fn candidate_pid(
    ctx: &Ctx,
    candidate: &(
        crate::core::registry::Effective,
        crate::core::bench::lifecycle::StepAction,
    ),
    runtime: Option<&RuntimeSpec>,
) -> Result<i32, ChekovError> {
    use crate::core::bench::lifecycle::StepAction;
    if runtime.is_some() {
        return Ok(0);
    }
    let (eff, action) = candidate;
    match action {
        StepAction::UseRunning => {
            crate::core::server::live_pid(&ctx.config).ok_or(ChekovError::ServerNotRunning)
        }
        StepAction::Launch | StepAction::Judge => candidate::launch(ctx, eff),
    }
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
    let pid = candidate_pid(ctx, candidate, inputs.args.runtime.as_ref())?;
    let setup = Candidate {
        eff: eff.clone(),
        pid,
    };
    let dir = measure_candidate(ctx, &setup, inputs)?;
    if matches!(action, StepAction::Launch | StepAction::Judge) {
        candidate::teardown(ctx, pid)?;
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
        codebase: inputs.prepared.map(|p| CodebaseHead {
            head: p.head.as_str(),
            set_hash: p.set_hash.as_str(),
            allow_exec: p.exec.allowed(),
            cargo_version: p.exec.cargo_version(),
        }),
        judge: inputs
            .judge
            .map(crate::core::bench::judge::JudgePlan::stamp),
        runtime: inputs.args.runtime.as_ref(),
    }
}

/// `chekov: runtime <stored> serves: <id>[, <id>...]` — chekov cannot know
/// how a foreign server names the weights, so it REPORTS and lets the human
/// read (spec §4). An empty list still prints and the run proceeds.
fn serves_line(spec: &RuntimeSpec, ids: &[String]) -> String {
    let served = if ids.is_empty() {
        "(none listed)".to_owned()
    } else {
        ids.join(", ")
    };
    format!("chekov: runtime {} serves: {served}", spec.stored())
}

/// Foreign readiness: one `GET /v1/models`, the served ids printed AND
/// returned (the caller resolves which one is the subject — §4, §5, finding
/// (a)), and a geometry chekov did not set left at zero rather than invented.
fn foreign_props(
    ctx: &Ctx,
    upstream: &crate::core::proxy::serve::Upstream,
    spec: &RuntimeSpec,
) -> Result<(crate::core::bench::runner::PropsInfo, Vec<String>), ChekovError> {
    let ids = runtime::foreign_ready(ctx.http.as_ref(), &upstream.base_url)?;
    println!("{}", serves_line(spec, &ids));
    let props = crate::core::bench::runner::PropsInfo {
        n_ctx: 0,
        total_slots: 0,
    };
    Ok((props, ids))
}

/// The model name the request wire is built with — the resolved served id
/// on a foreign run, chekov's own registry name on every other run. Never
/// the reverse: the registry name never reaches the request wire on a
/// foreign run, and the served id never reaches the stamp (finding (a)).
fn wire_model<'a>(served: Option<&'a str>, setup: &'a Candidate) -> &'a str {
    served.unwrap_or(&setup.eff.name)
}

/// Which FIM transport the codebase suite rides: llama.cpp's own `/infill`,
/// and chat completions for every foreign runtime (spec §6).
const fn fim_for(runtime: Option<&RuntimeSpec>) -> crate::core::bench::runner::FimTransport {
    use crate::core::bench::runner::FimTransport;
    match runtime {
        Some(_) => FimTransport::Chat,
        None => FimTransport::Infill,
    }
}

/// Whose clock times a run — and so which door its throughput rows record and
/// what the stamp claims (spec §5, §6).
///
/// llama.cpp reports a `timings` object chekov reads back; a foreign runtime
/// reports none, so chekov times the streamed reply itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimingClock {
    /// The server's own `timings` object, read off the buffered reply.
    Server,
    /// chekov's wall clock over the SSE stream.
    ChekovStreamed,
}

impl TimingClock {
    /// Selection is by runtime, and only by runtime — exactly like `fim_for`.
    const fn of(runtime: Option<&RuntimeSpec>) -> Self {
        match runtime {
            Some(_) => Self::ChekovStreamed,
            None => Self::Server,
        }
    }

    /// The door a throughput row is recorded under. A resumed run looks its
    /// recorded depths up by this key, so it must match what wrote them.
    const fn transport(self) -> crate::core::bench::store::Transport {
        use crate::core::bench::store::Transport;
        match self {
            Self::Server => Transport::Buffered,
            Self::ChekovStreamed => Transport::Streamed,
        }
    }

    /// What the stamp records as having measured the run.
    const fn source(self) -> &'static str {
        use crate::core::bench::stamp;
        match self {
            Self::Server => stamp::TIMING_SERVER,
            Self::ChekovStreamed => stamp::TIMING_CHEKOV_STREAMED,
        }
    }

    /// The crossing that measures one throughput probe under this clock.
    fn cross(
        self,
        wire: &crate::core::bench::runner::ProbeWire,
        req: &crate::core::proxy::http::HttpRequest,
    ) -> Result<crate::core::bench::runner::ProbeArtifact, ChekovError> {
        use crate::core::bench::runner;
        match self {
            Self::Server => runner::cross(wire, req),
            Self::ChekovStreamed => runner::cross_stream_timed(wire, req),
        }
    }
}

/// Readiness through rendering for one already-up server.
fn measure_candidate(
    ctx: &Ctx,
    setup: &Candidate,
    inputs: &RunInputs,
) -> Result<std::path::PathBuf, ChekovError> {
    use crate::core::bench::{store, sweep};
    use crate::core::proxy::serve::Upstream;
    let cfg = &ctx.config;
    let args = inputs.args;
    let upstream = Upstream {
        base_url: args.upstream.map_or_else(|| cfg.base_url(), str::to_owned),
        api_key: cfg.file.server.api_key.clone(),
    };
    let (props, served) = match args.runtime.as_ref() {
        Some(spec) => {
            let (props, ids) = foreign_props(ctx, &upstream, spec)?;
            (props, Some(runtime::served_model(args.served_model, &ids)?))
        }
        None => (candidate::ensure_ready(ctx, &upstream, setup)?, None),
    };
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
            model: wire_model(served.as_deref(), setup),
            fixture: args.fixture,
            suite: args.suite,
            prepared: inputs.prepared,
            fim: fim_for(args.runtime.as_ref()),
            clock: TimingClock::of(args.runtime.as_ref()),
            runtime: args.runtime.as_ref().map(RuntimeSpec::stored),
        },
    )?;
    print!("{}", store::render_run(&store::RunLog::load(writer.dir())?));
    Ok(writer.dir().to_path_buf())
}

/// Launch the judge once, judge every run directory, tear it down (spec C §3).
fn run_judge_phase(
    ctx: &Ctx,
    runs: &[std::path::PathBuf],
    plan: &crate::core::bench::judge::JudgePlan,
) -> Result<(), ChekovError> {
    use crate::core::bench::store::{RunLog, render_codebase};
    let pid = candidate::launch(ctx, &plan.judge)?;
    let (upstream, facade) = judge_wire_parts(ctx, plan);
    let ready = candidate::ensure_ready(
        ctx,
        &upstream,
        &Candidate {
            eff: plan.judge.clone(),
            pid,
        },
    );
    let wire = judge_wire(ctx, &upstream, &facade);
    let outcome = ready.and_then(|_| {
        runs.iter().try_for_each(|dir| {
            let verdicts = judge_run(&wire, dir, plan)?;
            eprintln!(
                "chekov bench: judge '{}' — {verdicts} verdict(s) for {}",
                plan.judge.name,
                dir.display()
            );
            print!("{}", render_codebase(&RunLog::load(dir)?));
            Ok::<(), ChekovError>(())
        })
    });
    candidate::teardown(ctx, pid)?;
    outcome
}

/// The two values the judge's wire borrows from — owned by the caller so the
/// wire can hold references to them.
fn judge_wire_parts(
    ctx: &Ctx,
    plan: &crate::core::bench::judge::JudgePlan,
) -> (
    crate::core::proxy::serve::Upstream,
    crate::core::proxy::claude::ClaudeFacade,
) {
    (
        crate::core::proxy::serve::Upstream {
            base_url: ctx.config.base_url(),
            api_key: ctx.config.file.server.api_key.clone(),
        },
        crate::core::proxy::claude::ClaudeFacade::new(&plan.judge.name),
    )
}

/// The judge's crossing wire: chekov's own translator, the same sampling pins
/// every probe carries (spec C §3.0 — one uniform judge wire).
fn judge_wire<'a>(
    ctx: &'a Ctx,
    upstream: &'a crate::core::proxy::serve::Upstream,
    facade: &'a crate::core::proxy::claude::ClaudeFacade,
) -> crate::core::bench::runner::ProbeWire<'a> {
    use crate::core::bench::runner::{ProbeWire, SamplingPins};
    ProbeWire {
        http: ctx.http.as_ref(),
        facade,
        upstream,
        pins: SamplingPins {
            seed: ctx.config.file.bench.seed,
        },
    }
}

/// The eval directory and the run id a run directory decomposes into — what
/// `RunWriter::resume` is addressed by. Neither half is ever guessed.
fn run_location(dir: &std::path::Path) -> Result<(&std::path::Path, &str), ChekovError> {
    let invalid = |reason: &str| ChekovError::BenchRunInvalid {
        path: dir.to_path_buf(),
        reason: reason.to_owned(),
    };
    let eval = dir.parent().ok_or_else(|| invalid("no parent directory"))?;
    let run_id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| invalid("no run id in the path"))?;
    Ok((eval, run_id))
}

/// Every eligible `function_body` crossing of one run, appended as it lands.
fn judge_run(
    wire: &crate::core::bench::runner::ProbeWire,
    dir: &std::path::Path,
    plan: &crate::core::bench::judge::JudgePlan,
) -> Result<usize, ChekovError> {
    use crate::core::bench::store::{self, RunLog, RunWriter, TaskKey};
    let log = RunLog::load(dir)?;
    let (eval, run_id) = run_location(dir)?;
    let (mut writer, _) = RunWriter::resume(eval, run_id, &log.head)?;
    let mut count = 0;
    for row in log
        .rows
        .iter()
        .filter(|r| r.suite == "codebase" && !store::is_unavailable(r))
    {
        if log.is_done(&TaskKey::buffered(store::JUDGE_SUITE, &row.task_id)) {
            continue;
        }
        let Some(codebase) = row.codebase.as_ref() else {
            continue;
        };
        let Some(judge_row) = verdict_for(wire, codebase, plan)? else {
            continue;
        };
        writer.append(store::Task {
            suite: store::JUDGE_SUITE.into(),
            task_id: row.task_id.clone(),
            measure: crate::core::bench::codebase::run::empty_measure(),
            grade: None,
            transport: store::Transport::Buffered,
            codebase: None,
            judge: Some(judge_row),
        })?;
        count += 1;
    }
    Ok(count)
}

/// One crossing's judge row, or `None` when the row is not a judge row at all.
fn verdict_for(
    wire: &crate::core::bench::runner::ProbeWire,
    row: &crate::core::bench::store::CodebaseRow,
    plan: &crate::core::bench::judge::JudgePlan,
) -> Result<Option<crate::core::bench::store::JudgeRow>, ChekovError> {
    use crate::core::bench::judge::{self, Eligibility};
    use crate::core::bench::store::{DecidedBy, JudgeRow};
    let settled = |equivalent, decided_by, skipped| JudgeRow {
        equivalent,
        gold_first: None,
        prediction_first: None,
        decided_by,
        skipped,
        judge_secs: 0.0,
    };
    let pair = match judge::eligibility(row) {
        None => return Ok(None),
        Some(Eligibility::Identical) => {
            return Ok(Some(settled(Some(true), DecidedBy::Identical, None)));
        }
        Some(Eligibility::Skipped(reason)) => {
            return Ok(Some(settled(
                None,
                DecidedBy::Skipped,
                Some(reason.to_owned()),
            )));
        }
        Some(Eligibility::Judge(pair)) => pair,
    };
    let schema = judge::schema();
    let ask = JudgeAsk {
        wire,
        forced: plan.forced(&schema),
        max_tokens: plan.max_tokens,
    };
    judged_verdict(&ask, &pair).map(Some)
}

/// A crossing nobody has decided: both orders asked, and what they settle to.
/// `judge_secs` is what the crossing cost — both orders, not one order's
/// share: the trailer's median is per crossing.
fn judged_verdict(
    ask: &JudgeAsk,
    pair: &crate::core::bench::judge::Pair,
) -> Result<crate::core::bench::store::JudgeRow, ChekovError> {
    use crate::core::bench::judge::{Reply, combine, requests};
    use crate::core::bench::store::JudgeRow;
    let [first, second] = requests(pair, ask.max_tokens);
    let started = std::time::Instant::now();
    let (gold_first, prediction_first) = (ask_judge(ask, &first)?, ask_judge(ask, &second)?);
    let verdict = combine(&gold_first, &prediction_first);
    let answer = |r: &Reply| match r {
        Reply::Answer(b) => Some(*b),
        Reply::Skipped(_) => None,
    };
    Ok(JudgeRow {
        equivalent: verdict.equivalent,
        gold_first: answer(&gold_first),
        prediction_first: answer(&prediction_first),
        decided_by: verdict.decided_by,
        skipped: verdict.skipped,
        judge_secs: started.elapsed().as_secs_f64(),
    })
}

/// One judge request's wire and what it forces, bundled (§4).
struct JudgeAsk<'a> {
    wire: &'a crate::core::bench::runner::ProbeWire<'a>,
    forced: crate::core::bench::runner::Forced<'a>,
    max_tokens: u32,
}

/// One order's reply. A judge server that answers a non-2xx is up and
/// reachable: its refusal skips THIS crossing (spec C §7) and the phase goes
/// on. Every other error stops the phase with the rows so far intact.
fn ask_judge(
    ask: &JudgeAsk,
    req: &crate::core::proxy::http::HttpRequest,
) -> Result<crate::core::bench::judge::Reply, ChekovError> {
    use crate::core::bench::judge::{Reply, parse_reply};
    match crate::core::bench::runner::cross_forced_with(ask.wire, req, &ask.forced) {
        Ok(artifact) => Ok(parse_reply(&artifact.anthropic_body, ask.max_tokens)),
        Err(refused @ ChekovError::UpstreamRefused { .. }) => {
            Ok(Reply::Skipped(format!("judge refused: {refused}")))
        }
        Err(other) => Err(other),
    }
}

/// What the suites need beyond the sink and `Ctx` (§4).
struct SuiteInputs<'a> {
    plan: &'a crate::core::bench::sweep::SweepPlan,
    upstream: &'a crate::core::proxy::serve::Upstream,
    model: &'a str,
    fixture: Option<&'a std::path::Path>,
    suite: Option<crate::core::bench::lifecycle::Suite>,
    prepared: Option<&'a crate::core::bench::codebase::Prepared>,
    /// Which wire the codebase suite fills its rows over — selected by
    /// runtime, and threaded down to the one crossing that sends it (§6).
    fim: crate::core::bench::runner::FimTransport,
    /// Whose clock times the throughput sweep — selected by runtime, and
    /// threaded down to the crossing and the row key it decides (§5).
    clock: TimingClock,
    /// The declared foreign runtime's stored spelling, when there is one —
    /// so a missing-timings failure names it instead of prescribing an
    /// engine rebuild that does not apply (C1).
    runtime: Option<String>,
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
    let runtime = inputs.runtime.as_deref();
    let recast = |e| runner::foreign_timings_error(e, runtime);
    let pass = SuitePass {
        wire: &wire,
        clock: inputs.clock,
        runtime,
    };
    if inputs.suite.is_some_and(Suite::runs_throughput) {
        run_throughput(sink, &pass, inputs.plan).map_err(recast)?;
    }
    if inputs.suite.is_some_and(Suite::runs_agentic) {
        run_agentic(sink, &pass).map_err(recast)?;
    }
    if let Some(path) = inputs.fixture {
        run_fixture(sink, &pass, path).map_err(recast)?;
    }
    run_codebase_suite(sink, inputs, &wire)
}

/// The codebase suite over its own sink — the wire it fills rows on is the
/// run's, the transport its own (§6). Nothing prepared, nothing to run.
fn run_codebase_suite(
    sink: &mut TaskSink,
    inputs: &SuiteInputs,
    wire: &crate::core::bench::runner::ProbeWire,
) -> Result<(), ChekovError> {
    use crate::core::bench::codebase::run;
    let Some(prepared) = inputs.prepared else {
        return Ok(());
    };
    run::run_codebase(
        &mut run::Sink {
            writer: sink.writer,
            done: sink.done,
            fim: inputs.fim,
            runtime: inputs.runtime.clone(),
        },
        wire,
        prepared,
    )
}

/// What every suite crossing needs beyond the sink: the wire it rides, whose
/// clock times it, and the declared foreign runtime whose name a
/// missing-timings row failure must carry (§5, §7). Bundled so each suite
/// runner stays inside the 3-argument limit (§4).
struct SuitePass<'a> {
    wire: &'a crate::core::bench::runner::ProbeWire<'a>,
    clock: TimingClock,
    runtime: Option<&'a str>,
}

/// A crossing outcome as its own row will read it.
///
/// `append_probe` swallows a failure into row text, so the recast has to
/// happen here rather than at the suite boundary: on a foreign run a
/// missing-timings failure names the declared runtime, and llama.cpp
/// outcomes pass through byte-for-byte unchanged (§7).
fn row_outcome<T>(
    outcome: Result<T, ChekovError>,
    runtime: Option<&str>,
) -> Result<T, ChekovError> {
    outcome.map_err(|e| crate::core::bench::runner::foreign_timings_error(e, runtime))
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
fn run_agentic(sink: &mut TaskSink, suite: &SuitePass) -> Result<(), ChekovError> {
    use crate::core::bench::store::Transport;
    let set = crate::core::bench::probeset::agentic_v0()?;
    // Once the engine has refused a forced grammar, it will refuse every
    // one: record the rest as unavailable rather than firing doomed
    // requests and calling each refusal a model failure.
    let mut refusal = None;
    for transport in [Transport::Buffered, Transport::Streamed] {
        let mut pass = AgenticPass {
            suite,
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

/// One door's pass over the agentic set: what every suite crossing rides,
/// which door, and the engine's refusal of forced grammars once it has
/// refused.
struct AgenticPass<'a> {
    suite: &'a SuitePass<'a>,
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
        let outcome = runner::cross_via(pass.suite.wire, &probes::tool_probe(case), pass.transport)
            .map(|artifact| {
                (
                    artifact.timings,
                    grade_row(grade::grade_tool_emit(&artifact.anthropic_body, case)),
                )
            });
        append_probe(sink, key, row_outcome(outcome, pass.suite.runtime))?;
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
    let outcome = runner::cross_via(
        pass.suite.wire,
        &probes::instruction_probe(case),
        pass.transport,
    )
    .map(|artifact| {
        let (strict, loose) = grade::grade_instruction(&artifact.anthropic_body, case);
        (artifact.timings, instruction_row(strict, &loose))
    });
    append_probe(sink, key, row_outcome(outcome, pass.suite.runtime))
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
    let crossed = runner::cross_forced(forced.suite.wire, &probes::forced_probe(case), &schema);
    match row_outcome(crossed, forced.suite.runtime) {
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
        judge: None,
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
        judge: None,
    })
}

/// Create a fresh run, or reopen one for `--resume` (stamp must match).
fn open_run(
    ctx: &Ctx,
    head: &crate::core::bench::store::RunHead,
    resume: Option<&str>,
) -> Result<(crate::core::bench::store::RunWriter, Vec<Done>), ChekovError> {
    use crate::core::bench::store::{self, RunWriter};
    let eval = ctx.config.eval_dir();
    if let Some(run_id) = resume {
        // A run recorded before it had a judge takes this one — but only when
        // the rest of the stamp already matches, so a resume `RunWriter::resume`
        // is about to refuse never rewrites the run it refused.
        store::adopt_judge(&eval.join(run_id), &head.stamp)?;
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
    pass: &SuitePass,
    plan: &crate::core::bench::sweep::SweepPlan,
) -> Result<(), ChekovError> {
    use crate::core::bench::{store, sweep};
    let transport = pass.clock.transport();
    for &depth in &plan.depths {
        let task_id = format!("depth-{depth}");
        if sink.is_done(&store::TaskKey {
            suite: "throughput",
            task_id: &task_id,
            transport,
        }) {
            eprintln!("chekov: {task_id} already recorded — skipped (--resume)");
            continue;
        }
        let result =
            sweep::measure_depth(plan, depth, &mut |req| pass.clock.cross(pass.wire, req))?;
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
            transport,
            codebase: None,
            judge: None,
        })?;
    }
    Ok(())
}

/// Cross and grade every fixture probe. A crossing failure records a FAIL
/// with its reason — a broken exchange must never look like an empty reply.
fn run_fixture(
    sink: &mut TaskSink,
    pass: &SuitePass,
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
        let outcome = runner::cross(pass.wire, &probes::fixture_probe(probe)).map(|artifact| {
            (
                artifact.timings,
                grade_row(grade::grade(&artifact.anthropic_body, probe)),
            )
        });
        append_probe(
            sink,
            TaskKey::buffered("fixture", &probe.id),
            row_outcome(outcome, pass.runtime),
        )?;
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

/// The codebase run's identity and the environment its exec tiers ran in.
struct CodebaseHead<'a> {
    head: &'a str,
    set_hash: &'a str,
    allow_exec: bool,
    cargo_version: Option<&'a str>,
}

/// Everything the stamp is built from beyond `Ctx` and the setup (§4).
struct HeadInputs<'a> {
    props: crate::core::bench::runner::PropsInfo,
    plan: &'a crate::core::bench::sweep::SweepPlan,
    fixture: Option<&'a std::path::Path>,
    suite: Option<crate::core::bench::lifecycle::Suite>,
    /// The codebase run, when there is one — drives `corpus_id` ahead of
    /// `suite`/`fixture`, and the three exec fields.
    codebase: Option<CodebaseHead<'a>>,
    /// The judge this run's `equiv` column will be measured with, when
    /// `--judge` was given — the instrument, its budget and its floor.
    judge: Option<crate::core::bench::stamp::JudgeStamp>,
    /// The declared foreign runtime, when there is one — what turns the
    /// engine lookup, the flag sextet and the FIM hash foreign (§5, §6).
    runtime: Option<&'a RuntimeSpec>,
}

impl HeadInputs<'_> {
    /// Whether this run was allowed to execute the repository. A run that
    /// executed it and one that only read it are not the same environment.
    fn allow_exec(&self) -> bool {
        self.codebase.as_ref().is_some_and(|c| c.allow_exec)
    }

    /// The `cargo --version` line, when exec actually ran — `None` both
    /// without the flag and on a machine with no toolchain.
    fn cargo_version(&self) -> Option<&str> {
        self.codebase.as_ref().and_then(|c| c.cargo_version)
    }
}

/// WHICH machine measured: the hashed identity and its human-readable brand.
/// Required on every path, foreign included — chekov always knows its own
/// hardware, and a stamp cannot pin an unknown one.
fn machine_identity(
    cfg: &crate::core::config::Config,
) -> Result<(String, Option<String>), ChekovError> {
    let probed = machine::probe(&cfg.engine_dir());
    let machine_id = machine::machine_id(&probed).ok_or_else(|| ChekovError::SetupIncomplete {
        remaining: "the machine identity is incomplete (model, memory, chip, or GPU \
                    cores unknown) — run `chekov setup` and retry"
            .to_owned(),
    })?;
    Ok((machine_id, probed.chip))
}

/// Who measured: the machine identity, its brand, and the engine commit. Each
/// is required — a stamp cannot pin an unknown. The foreign path does not come
/// through here: it has no chekov-built engine to look up (spec §5).
pub(crate) fn stamp_identity(
    cfg: &crate::core::config::Config,
) -> Result<(String, Option<String>, String), ChekovError> {
    let (machine_id, brand) = machine_identity(cfg)?;
    let engine = crate::core::engine::recorded_commit(&cfg.logs_dir())
        .or_else(|| crate::core::engine::current_commit(&cfg.engine_dir()))
        .ok_or_else(|| ChekovError::SetupIncomplete {
            remaining: "the engine commit is unknown — run `chekov update --engine` so \
                        the stamp can pin it"
                .to_owned(),
        })?;
    Ok((machine_id, brand, engine))
}

/// The stamp's prompt-set hash and corpus id — split out since both read
/// `inputs.suite` and the seed together (§4, and keeps `build_head` short).
fn head_corpus(
    inputs: &HeadInputs,
    bench_cfg: &crate::core::config::BenchSection,
) -> Result<(String, String), ChekovError> {
    use crate::core::bench::probes;
    let base = inputs.suite.map_or_else(
        || "codebase-only".to_owned(),
        |suite| probes::suite_prompt_hash(suite, inputs.plan, bench_cfg.seed),
    );
    let prompt_set_hash = wrapped_prompt_hash(inputs, base);
    let corpus = match inputs.codebase.as_ref() {
        Some(c) => codebase_corpus_id(c.head, c.set_hash),
        None => corpus_id(inputs.suite, inputs.fixture)?,
    };
    Ok((prompt_set_hash, corpus))
}

/// The chat FIM template is part of the prompt set, so a codebase suite that
/// rides the chat arm hashes differently from one that rides `/infill` — a
/// template edit is then a NAMED stamp change, and the two transports can
/// never carry the same hash (spec §6). A foreign run with no codebase suite
/// sends no FIM prompt at all, and keeps today's value.
fn wrapped_prompt_hash(inputs: &HeadInputs, base: String) -> String {
    if inputs.runtime.is_some() && inputs.codebase.is_some() {
        return crate::core::bench::runner::chat_fim_hash(&base);
    }
    base
}

/// The stamp's remaining pieces once identity, flags and corpus are known —
/// bundled so `assemble_stamp` stays within the 3-argument limit (§4).
struct StampParts {
    machine_id: String,
    /// `"llama.cpp"`, or the declared `<name> <version>` (spec §3).
    runtime: String,
    engine: String,
    prompt_set_hash: String,
    corpus_id: String,
    flags: StampedFlags,
    seed: u32,
}

/// The stamp itself, given the setup and inputs `build_head` already has plus
/// everything else bundled in `parts`.
fn assemble_stamp(
    setup: &Candidate,
    inputs: &HeadInputs,
    parts: StampParts,
) -> crate::core::bench::stamp::Stamp {
    use crate::core::bench::stamp;
    stamp::Stamp {
        machine_id: parts.machine_id,
        runtime: parts.runtime,
        // Whoever timed the crossings is whoever the suites asked: one
        // selection, by runtime, spelled once (§5, §6).
        timing_source: TimingClock::of(inputs.runtime).source().to_owned(),
        engine_build_commit: parts.engine,
        weights_revision: format!(
            "{}/{}",
            setup.eff.entry.revision, setup.eff.entry.first_shard
        ),
        quant: setup.eff.entry.quant.clone(),
        ctx: inputs.props.n_ctx,
        n_parallel: inputs.props.total_slots,
        kv_unified: parts.flags.kv_unified,
        n_batch: parts.flags.n_batch,
        n_ubatch: parts.flags.n_ubatch,
        type_k: parts.flags.type_k,
        type_v: parts.flags.type_v,
        flash_attn: parts.flags.flash_attn,
        allow_exec: inputs.allow_exec(),
        cargo_version: inputs.cargo_version().map(str::to_owned),
        // The scratch target exists exactly when a toolchain answered the
        // probe — the same condition that put a version on the stamp.
        exec_target: if inputs.cargo_version().is_some() {
            stamp::EXEC_TARGET_SCRATCH.to_owned()
        } else {
            stamp::EXEC_TARGET_OFF.to_owned()
        },
        seed: parts.seed,
        temperature_milli: 0,
        chekov_version: env!("CARGO_PKG_VERSION").to_owned(),
        prompt_set_hash: parts.prompt_set_hash,
        corpus_id: parts.corpus_id,
        judge: inputs.judge.clone(),
    }
}

/// Who and what measured, as the head records it: the machine, the build
/// string, the argv chekov passed, the flags read back out of it, and the
/// runtime's stored name. Bundled so `build_head` stays one assembly (§4).
struct HeadIdentity {
    machine_id: String,
    machine_brand: Option<String>,
    engine: String,
    launch_args: Vec<String>,
    flags: StampedFlags,
    runtime: String,
}

/// chekov's own llama.cpp: a probed engine commit and the real argv it will
/// launch the server with, exactly as before this flag existed.
fn local_identity(ctx: &Ctx, setup: &Candidate) -> Result<HeadIdentity, ChekovError> {
    let cfg = &ctx.config;
    let (machine_id, machine_brand, engine) = stamp_identity(cfg)?;
    let launch_args = crate::core::server::launch_args(cfg, &setup.eff);
    let flags = stamped_flags(&launch_args);
    Ok(HeadIdentity {
        machine_id,
        machine_brand,
        engine,
        launch_args,
        flags,
        runtime: crate::core::bench::stamp::RUNTIME_LLAMA_CPP.to_owned(),
    })
}

/// A server chekov did not launch: the declared version IS the build string,
/// the argv is empty because chekov passed none, and every flag is the
/// `unmanaged` sentinel (spec §5).
fn foreign_identity(ctx: &Ctx, spec: &RuntimeSpec) -> Result<HeadIdentity, ChekovError> {
    let (machine_id, machine_brand) = machine_identity(&ctx.config)?;
    let (engine, flags) = foreign_stamp_parts(spec);
    Ok(HeadIdentity {
        machine_id,
        machine_brand,
        engine,
        launch_args: Vec::new(),
        flags,
        runtime: spec.stored(),
    })
}

/// The declared runtime's build identity and its sentinel flag sextet — the
/// pure half of the foreign head, so it can be pinned without a machine.
fn foreign_stamp_parts(spec: &RuntimeSpec) -> (String, StampedFlags) {
    (spec.version.clone(), unmanaged_flags())
}

fn build_head(
    ctx: &Ctx,
    setup: &Candidate,
    inputs: &HeadInputs,
) -> Result<crate::core::bench::store::RunHead, ChekovError> {
    use crate::core::bench::store;
    let identity = match inputs.runtime {
        Some(spec) => foreign_identity(ctx, spec)?,
        None => local_identity(ctx, setup)?,
    };
    let bench_cfg = &ctx.config.file.bench;
    let (prompt_set_hash, corpus_id) = head_corpus(inputs, bench_cfg)?;
    let head_stamp = assemble_stamp(
        setup,
        inputs,
        StampParts {
            machine_id: identity.machine_id,
            runtime: identity.runtime,
            engine: identity.engine,
            prompt_set_hash,
            corpus_id,
            flags: identity.flags,
            seed: bench_cfg.seed,
        },
    );
    // Only a run with a forced pass has a forced reasoning mode to record.
    let forced_reasoning_format = inputs
        .suite
        .is_some_and(crate::core::bench::lifecycle::Suite::runs_agentic)
        .then(|| crate::core::bench::runner::FORCED_REASONING_FORMAT.to_owned());
    Ok(store::RunHead {
        model: setup.eff.name.clone(),
        machine_brand: identity.machine_brand,
        launch_args: identity.launch_args,
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

/// A flag on a server chekov did not launch: not observed, not invented —
/// a third spelling distinct from "engine-default" (spec §5).
const FLAG_UNMANAGED: &str = "unmanaged";

/// Every launch flag of a foreign server, all six unobservable (spec §5).
fn unmanaged_flags() -> StampedFlags {
    let sentinel = || FLAG_UNMANAGED.to_owned();
    StampedFlags {
        kv_unified: sentinel(),
        n_batch: sentinel(),
        n_ubatch: sentinel(),
        type_k: sentinel(),
        type_v: sentinel(),
        flash_attn: sentinel(),
    }
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

/// Which two stored runs to compare, and under what rules (spec §7).
struct CompareArgs<'a> {
    a: &'a std::path::Path,
    b: &'a std::path::Path,
    cross_runtime: bool,
}

const fn compare_args<'a>(
    a: &'a std::path::Path,
    b: &'a std::path::Path,
    cross_runtime: bool,
) -> CompareArgs<'a> {
    CompareArgs {
        a,
        b,
        cross_runtime,
    }
}

fn compare(ctx: &Ctx, args: &CompareArgs) -> Result<ExitCode, ChekovError> {
    use crate::core::bench::{compare as bench_compare, store};
    let run_a = store::RunLog::load(&resolve_run(ctx, args.a))?;
    let run_b = store::RunLog::load(&resolve_run(ctx, args.b))?;
    let opts = bench_compare::CompareOpts {
        significance_pct: f64::from(ctx.config.file.bench.significance_pct),
        cross_runtime: args.cross_runtime,
    };
    let comparison = bench_compare::compare_runs(&run_a, &run_b, &opts)?;
    if opts.cross_runtime {
        print!(
            "{}",
            bench_compare::cross_runtime_banner(&run_a.head.stamp, &run_b.head.stamp)
        );
    }
    print!(
        "{}",
        bench_compare::render_comparison(
            &bench_compare::RunPair {
                a: &run_a,
                b: &run_b
            },
            &comparison
        )
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::{ChekovError, Machine, render_json, render_scan};
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

    /// The nextn row stops being a bare number: a weights file that carries a
    /// native MTP draft head says so, and says the engine decodes without it —
    /// the latent speedup is a fact about the model worth naming. A file
    /// without one keeps the plain row.
    #[test]
    fn explain_names_the_mtp_head_when_the_weights_carry_one() {
        let with = crate::core::gguf::Geometry {
            arch: "qwen35moe".into(),
            block_count: Some(41),
            nextn_predict_layers: Some(1),
            ..Default::default()
        };
        let out = super::render_explain(&super::Explained {
            name: "m",
            geometry: &with,
            ctx_len: 4096,
            weights: None,
            q8_cache: false,
        });
        assert!(
            out.contains(
                "nextn_predict_layers    1   (a native MTP draft head; this engine \
                 decodes without it)"
            ),
            "{out}"
        );

        let without = crate::core::gguf::Geometry {
            nextn_predict_layers: None,
            ..with
        };
        let out = super::render_explain(&super::Explained {
            name: "m",
            geometry: &without,
            ctx_len: 4096,
            weights: None,
            q8_cache: false,
        });
        assert!(out.contains("nextn_predict_layers    0\n"), "{out}");
        assert!(!out.contains("MTP"), "{out}");
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
    fn compare_cross_runtime_flag_is_a_bare_switch_defaulting_off() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from([
            "chekov",
            "capability",
            "compare",
            "a.json",
            "b.json",
        ])
        .expect("compare parses");
        match cli.cmd {
            crate::cli::Cmd::Capability(cap) => match cap.action {
                Some(super::CapAction::Compare { cross_runtime, .. }) => {
                    assert!(!cross_runtime);
                }
                other => panic!("expected Compare, got {other:?}"),
            },
            _ => panic!("expected capability"),
        }
        let cli = crate::cli::Cli::try_parse_from([
            "chekov",
            "capability",
            "compare",
            "a.json",
            "b.json",
            "--cross-runtime",
        ])
        .expect("compare parses with the flag");
        match cli.cmd {
            crate::cli::Cmd::Capability(cap) => match cap.action {
                Some(super::CapAction::Compare { cross_runtime, .. }) => {
                    assert!(cross_runtime);
                }
                other => panic!("expected Compare, got {other:?}"),
            },
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

    #[test]
    fn allow_exec_defaults_off_and_is_a_bare_switch() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            opts: super::BenchOpts,
        }

        let off = Wrap::parse_from(["bench", "--codebase", "."]);
        assert!(
            !off.opts.allow_exec,
            "nothing executes unless it is asked for"
        );
        let on = Wrap::parse_from(["bench", "--codebase", ".", "--allow-exec"]);
        assert!(on.opts.allow_exec);
    }

    /// The declared runtime every foreign test speaks about.
    fn foreign_spec() -> crate::core::bench::runtime::RuntimeSpec {
        crate::core::bench::runtime::RuntimeSpec::parse("mtplx@0.4.1").expect("parses")
    }

    /// A registered model shaped only enough to be stamped — `UseRunning`
    /// needs no shard on disk, and the stamp reads the entry, not the file.
    fn foreign_candidate() -> crate::core::bench::candidate::Candidate {
        crate::core::bench::candidate::Candidate {
            eff: crate::core::registry::Effective {
                name: "mtplx-model".into(),
                ctx_size: 4096,
                flags: vec![],
                entry: crate::core::registry::ModelEntry {
                    repo: "acme/Model-GGUF".into(),
                    quant: "UD-Q5_K_XL".into(),
                    revision: "abc123def456".into(),
                    path: "models/mtplx-model@abc123def456".into(),
                    first_shard: "Model-UD-Q5_K_XL-00001-of-00001.gguf".into(),
                    hermes_ok: true,
                    ctx_size: None,
                    extra_flags: vec![],
                    role: None,
                },
            },
            pid: 0,
        }
    }

    /// `--upstream` is meaningless without a declared runtime, and a
    /// malformed `--runtime` refuses at argument assembly — before a
    /// repository, a registry or a server is asked about.
    #[test]
    fn runtime_parses_and_upstream_requires_it() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            opts: super::BenchOpts,
        }

        let w = Wrap::parse_from([
            "cap",
            "--runtime",
            "mtplx@0.4.1",
            "--upstream",
            "http://127.0.0.1:9999",
        ]);
        assert_eq!(w.opts.runtime.as_deref(), Some("mtplx@0.4.1"));
        assert_eq!(w.opts.upstream.as_deref(), Some("http://127.0.0.1:9999"));
        assert!(
            Wrap::try_parse_from(["cap", "--upstream", "http://x"]).is_err(),
            "--upstream alone declares nothing"
        );

        let good = Wrap::parse_from(["cap", "--runtime", "mtplx@0.4.1"]);
        let args = super::bench_args(&good.opts).expect("a well-formed runtime");
        assert_eq!(
            args.runtime.as_ref().map(super::RuntimeSpec::stored),
            Some("mtplx 0.4.1".to_owned())
        );
        let bad = Wrap::parse_from(["cap", "--runtime", "MTPLX@1"]);
        assert!(matches!(
            super::bench_args(&bad.opts),
            Err(ChekovError::RuntimeFlagInvalid { .. })
        ));
    }

    /// `--served-model` is meaningless without a declared runtime, and
    /// parses alongside it (spec finding (a)).
    #[test]
    fn served_model_parses_with_runtime_and_is_refused_alone() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            opts: super::BenchOpts,
        }

        let w = Wrap::parse_from([
            "cap",
            "--runtime",
            "mtplx@0.4.1",
            "--served-model",
            "served-id",
        ]);
        assert_eq!(w.opts.served_model.as_deref(), Some("served-id"));
        let args = super::bench_args(&w.opts).expect("well-formed");
        assert_eq!(args.served_model, Some("served-id"));

        assert!(
            Wrap::try_parse_from(["cap", "--served-model", "served-id"]).is_err(),
            "--served-model alone declares no runtime"
        );
    }

    /// The binding fact: a foreign run's facade is built with the resolved
    /// served id, never the registry name, while the run/stamp naming keeps
    /// the registry name (finding (a)).
    #[test]
    fn the_wire_model_is_the_served_id_on_a_foreign_run_and_the_registry_name_otherwise() {
        let setup = foreign_candidate();
        assert_eq!(
            super::wire_model(Some("served-id"), &setup),
            "served-id",
            "the registry name never reaches the request wire on a foreign run"
        );
        assert_eq!(
            super::wire_model(None, &setup),
            "mtplx-model",
            "with nothing served to resolve, the registry name is what's left"
        );
    }

    /// The foreign path never launches the subject, so it can only ever
    /// measure one already-served model — and the refusal is pure: no `Ctx`,
    /// no server, no HTTP.
    #[test]
    fn a_foreign_run_with_two_models_is_refused_before_any_http() {
        use crate::core::bench::lifecycle::StepAction;
        let spec = foreign_spec();
        let two = ["a".to_owned(), "b".to_owned()];
        let err = super::foreign_actions(&spec, &two).expect_err("two subjects");
        assert!(
            matches!(err, ChekovError::RuntimeNeedsRunningServer { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("mtplx 0.4.1"), "{err}");
        let one = ["a".to_owned()];
        assert_eq!(
            super::foreign_actions(&spec, &one).expect("one served subject"),
            vec![StepAction::UseRunning]
        );
    }

    /// A server chekov did not launch has no observed flags and no observed
    /// geometry: every one of them is stamped `unmanaged` or zero, and the
    /// runtime is named rather than assumed (spec §5).
    #[test]
    fn a_foreign_stamp_is_sentinelled_and_named() {
        let spec = foreign_spec();
        let (engine, flags) = super::foreign_stamp_parts(&spec);
        assert_eq!(engine, "0.4.1", "the declared version IS the build string");
        for value in [
            &flags.kv_unified,
            &flags.n_batch,
            &flags.n_ubatch,
            &flags.type_k,
            &flags.type_v,
            &flags.flash_attn,
        ] {
            assert_eq!(value, "unmanaged", "not observed, and never invented");
        }
        let plan = plan_fixture();
        let stamp = foreign_stamp(&spec, &plan, (engine, flags));
        assert_eq!(stamp.ctx, 0);
        assert_eq!(stamp.n_parallel, 0);
        assert_eq!(stamp.runtime, "mtplx 0.4.1");
    }

    /// The stamp `build_head`'s foreign branch assembles, given the parts its
    /// own helper produced — the geometry chekov never observed stays zero.
    fn foreign_stamp(
        spec: &super::RuntimeSpec,
        plan: &crate::core::bench::sweep::SweepPlan,
        parts: (String, super::StampedFlags),
    ) -> crate::core::bench::stamp::Stamp {
        let (engine, flags) = parts;
        let inputs = super::HeadInputs {
            props: crate::core::bench::runner::PropsInfo {
                n_ctx: 0,
                total_slots: 0,
            },
            plan,
            fixture: None,
            suite: None,
            codebase: None,
            judge: None,
            runtime: Some(spec),
        };
        super::assemble_stamp(
            &foreign_candidate(),
            &inputs,
            super::StampParts {
                machine_id: "m".to_owned(),
                runtime: spec.stored(),
                engine,
                prompt_set_hash: "h".to_owned(),
                corpus_id: "c".to_owned(),
                flags,
                seed: 7,
            },
        )
    }

    /// Selection is by runtime, and only by runtime (spec §6).
    #[test]
    fn the_chat_arm_is_selected_exactly_for_a_foreign_runtime() {
        use crate::core::bench::runner::FimTransport;
        assert_eq!(super::fim_for(None), FimTransport::Infill);
        let spec = foreign_spec();
        assert_eq!(super::fim_for(Some(&spec)), FimTransport::Chat);
    }

    /// A foreign run is timed by chekov over the stream, so its throughput
    /// rows record the streamed door — and a resume must look for them there,
    /// or it re-runs every depth the run already holds (spec §5).
    #[test]
    fn a_foreign_throughput_row_is_streamed_and_resume_sees_it() {
        use crate::core::bench::store::{TaskKey, Transport};
        let spec = foreign_spec();
        let local = super::TimingClock::of(None);
        let foreign = super::TimingClock::of(Some(&spec));
        assert_eq!(local, super::TimingClock::Server);
        assert_eq!(foreign, super::TimingClock::ChekovStreamed);
        assert_eq!(local.transport(), Transport::Buffered);
        assert_eq!(foreign.transport(), Transport::Streamed);

        let recorded = vec![(
            "throughput".to_owned(),
            "depth-1024".to_owned(),
            Transport::Streamed,
        )];
        assert!(super::already_done(
            &recorded,
            &TaskKey {
                suite: "throughput",
                task_id: "depth-1024",
                transport: foreign.transport(),
            }
        ));
        assert!(
            !super::already_done(&recorded, &TaskKey::buffered("throughput", "depth-1024")),
            "today's buffered key would re-run every depth a foreign resume holds"
        );
    }

    /// `run_fixture` now rides the same clock as `run_throughput`: a foreign
    /// run's fixture crossings land on the streamed door, llama.cpp's stay
    /// on the buffered one it always used — red until `run_fixture` reads
    /// `pass.clock` instead of a hard-coded buffered key (spec §5, §7).
    #[test]
    fn a_foreign_fixture_row_is_streamed_and_resume_sees_it() {
        use crate::core::bench::store::{TaskKey, Transport};
        let spec = foreign_spec();
        let local = super::TimingClock::of(None);
        let foreign = super::TimingClock::of(Some(&spec));
        assert_eq!(
            local.transport(),
            Transport::Buffered,
            "llama.cpp's fixture door is unchanged"
        );
        assert_eq!(
            foreign.transport(),
            Transport::Streamed,
            "a foreign run's fixture door is timed by chekov, exactly like throughput"
        );

        let recorded = vec![(
            "fixture".to_owned(),
            "probe-1".to_owned(),
            Transport::Streamed,
        )];
        assert!(super::already_done(
            &recorded,
            &TaskKey {
                suite: "fixture",
                task_id: "probe-1",
                transport: foreign.transport(),
            }
        ));
        assert!(
            !super::already_done(&recorded, &TaskKey::buffered("fixture", "probe-1")),
            "today's buffered key would re-run every fixture probe a foreign resume holds"
        );
    }

    /// Whose clock measured the run is the stamp's own claim: chekov's on a
    /// foreign run, the server's on every run chekov launches (spec §6).
    #[test]
    fn the_stamp_names_chekovs_clock_exactly_on_foreign_runs() {
        use crate::core::bench::stamp::{TIMING_CHEKOV_STREAMED, TIMING_SERVER};
        let spec = foreign_spec();
        let plan = plan_fixture();
        let foreign = foreign_stamp(&spec, &plan, super::foreign_stamp_parts(&spec));
        assert_eq!(foreign.timing_source, TIMING_CHEKOV_STREAMED);
        assert_eq!(local_stamp(&plan).timing_source, TIMING_SERVER);
    }

    /// The stamp `build_head`'s local branch assembles: the same assembly with
    /// no declared runtime.
    fn local_stamp(
        plan: &crate::core::bench::sweep::SweepPlan,
    ) -> crate::core::bench::stamp::Stamp {
        let inputs = super::HeadInputs {
            props: crate::core::bench::runner::PropsInfo {
                n_ctx: 4096,
                total_slots: 1,
            },
            plan,
            fixture: None,
            suite: None,
            codebase: None,
            judge: None,
            runtime: None,
        };
        super::assemble_stamp(
            &foreign_candidate(),
            &inputs,
            super::StampParts {
                machine_id: "m".to_owned(),
                runtime: crate::core::bench::stamp::RUNTIME_LLAMA_CPP.to_owned(),
                engine: "dda1b0d67".to_owned(),
                prompt_set_hash: "h".to_owned(),
                corpus_id: "c".to_owned(),
                flags: super::unmanaged_flags(),
                seed: 7,
            },
        )
    }

    /// A foreign row's own FAIL text names the runtime that gave chekov
    /// nothing to time — never llama.cpp's rebuild advice, which cannot apply
    /// to a server chekov did not build (spec §7).
    #[test]
    fn a_foreign_agentic_row_failure_names_the_runtime_not_the_engine() {
        let outcome: Result<(), ChekovError> = Err(ChekovError::BenchNoTimings);
        let foreign = super::row_outcome(outcome, Some("mtplx 0.4.1")).expect_err("the crossing");
        let (_, row) = super::failed_probe(&foreign);
        let reason = row.reason.expect("a failed row carries its reason");
        assert!(reason.contains("mtplx 0.4.1"), "{reason}");
        assert!(!reason.contains("chekov update --engine"), "{reason}");

        let untouched: Result<(), ChekovError> = Err(ChekovError::BenchNoTimings);
        let local = super::row_outcome(untouched, None).expect_err("the crossing");
        assert_eq!(local.to_string(), ChekovError::BenchNoTimings.to_string());
    }

    fn timings_fixture() -> crate::core::bench::runner::Timings {
        crate::core::bench::runner::Timings {
            prompt_n: 10,
            prompt_per_second: 100.0,
            predicted_n: 20,
            predicted_per_second: 50.0,
            cache_n: 0,
        }
    }

    /// The foreign agentic BUFFERED door: a crossing that succeeded but was
    /// never timed appends the empty measure — never an invented zero
    /// `Timings` — alongside its real grade. Red until `append_probe` widens
    /// to `Option<Timings>` and an untimed success stops being impossible to
    /// express (spec §7.2).
    #[test]
    fn a_graded_buffered_foreign_row_carries_the_empty_measure_and_a_real_grade() {
        use crate::core::bench::store;
        let outcome: Result<(Option<crate::core::bench::runner::Timings>, _), ChekovError> =
            Ok((None, store::GradeRow::pass()));
        let (measure, grade) = super::outcome_row(outcome);
        assert_eq!(
            measure.prompt_n, 0,
            "no crossing was timed, so nothing is invented"
        );
        assert!(measure.decode_samples.is_empty());
        assert!(measure.prefill_samples.is_empty());
        assert_eq!(measure.cache_n, 0);
        assert!(grade.pass, "grading never depends on whether timing ran");
        assert!(
            !grade.unavailable,
            "an untimed success is graded, not unavailable"
        );
    }

    /// The foreign agentic STREAMED door (and every llama.cpp door): a timed
    /// crossing's real `Timings` become the row's real measure.
    #[test]
    fn a_graded_streamed_foreign_row_carries_derived_timings() {
        use crate::core::bench::store;
        let timings = timings_fixture();
        let outcome: Result<(Option<crate::core::bench::runner::Timings>, _), ChekovError> =
            Ok((Some(timings), store::GradeRow::pass()));
        let (measure, grade) = super::outcome_row(outcome);
        assert_eq!(measure.prompt_n, timings.prompt_n);
        assert_eq!(measure.decode_samples, vec![timings.predicted_per_second]);
        assert_eq!(measure.prefill_samples, vec![timings.prompt_per_second]);
        assert_eq!(measure.cache_n, timings.cache_n);
        assert!(
            !measure.decode_samples.is_empty(),
            "a timed crossing must never collapse to the untimed shape"
        );
        assert!(grade.pass);
    }

    /// `serves_line` reports what a foreign server names its weights on both
    /// branches — chekov cannot verify the list, so it prints and lets the
    /// human read (spec §4, M2).
    #[test]
    fn serves_line_joins_ids_or_says_none_listed() {
        let spec = foreign_spec();
        assert_eq!(
            super::serves_line(&spec, &["a".to_owned(), "b".to_owned()]),
            "chekov: runtime mtplx 0.4.1 serves: a, b"
        );
        assert_eq!(
            super::serves_line(&spec, &[]),
            "chekov: runtime mtplx 0.4.1 serves: (none listed)"
        );
    }

    /// A codebase suite that rides the chat arm carries a DIFFERENT
    /// prompt-set hash: the template is part of the prompt set, so a template
    /// edit is a named stamp change and the two transports never match.
    #[test]
    fn a_foreign_codebase_hash_wraps_the_base() {
        use crate::core::bench::lifecycle::Suite;
        let plan = plan_fixture();
        let bench_cfg = crate::core::config::BenchSection::default();
        let local = super::HeadInputs {
            props: props_fixture(),
            plan: &plan,
            fixture: None,
            suite: Some(Suite::Throughput),
            codebase: Some(super::CodebaseHead {
                head: "4818813deeaa",
                set_hash: "abcdef123456",
                allow_exec: false,
                cargo_version: None,
            }),
            judge: None,
            runtime: None,
        };
        let (base, corpus) = super::head_corpus(&local, &bench_cfg).expect("local hash");
        let spec = foreign_spec();
        let foreign = super::HeadInputs {
            runtime: Some(&spec),
            ..local
        };
        let (wrapped, foreign_corpus) =
            super::head_corpus(&foreign, &bench_cfg).expect("chat hash");
        assert_eq!(
            wrapped,
            crate::core::bench::runner::chat_fim_hash(&base),
            "the chat arm's hash wraps today's value"
        );
        assert_eq!(foreign_corpus, corpus, "the corpus is the same task set");
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
            byte_range: 9..19,
            gold: "let a = 1;".into(),
            prefix: "fn f() {\n".into(),
            suffix: "\n}\n".into(),
            excluded: Excluded {
                doc_comment: 0,
                cross_file: "n/a: same-file".into(),
                cfg_test_lines: 0,
                cross_file_withheld: 0,
            },
            name: None,
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
            exec: crate::core::bench::codebase::exec::Exec::Off,
        }
    }

    fn props_fixture() -> crate::core::bench::runner::PropsInfo {
        crate::core::bench::runner::PropsInfo {
            n_ctx: 4096,
            total_slots: 1,
        }
    }

    fn plan_fixture() -> crate::core::bench::sweep::SweepPlan {
        crate::core::bench::sweep::SweepPlan {
            depths: vec![],
            repetitions: 1,
            max_tokens: 64,
        }
    }

    /// The stamp says what the run was allowed to do, and — when it did it —
    /// which toolchain did it.
    #[test]
    fn the_head_records_the_exec_environment_only_when_exec_ran() {
        let plan = plan_fixture();
        let off = super::HeadInputs {
            props: props_fixture(),
            plan: &plan,
            fixture: None,
            suite: None,
            codebase: Some(super::CodebaseHead {
                head: "4818813deeaa",
                set_hash: "abcdef123456",
                allow_exec: false,
                cargo_version: None,
            }),
            judge: None,
            runtime: None,
        };
        assert!(!off.allow_exec());
        assert_eq!(off.cargo_version(), None);

        let on = super::HeadInputs {
            codebase: Some(super::CodebaseHead {
                head: "4818813deeaa",
                set_hash: "abcdef123456",
                allow_exec: true,
                cargo_version: Some("cargo 1.95.0"),
            }),
            ..off
        };
        assert!(on.allow_exec());
        assert_eq!(on.cargo_version(), Some("cargo 1.95.0"));
    }

    #[test]
    fn the_plan_line_and_the_estimate_both_name_the_exec_cost() {
        let prepared = prepared_counts(12, 6, 6);
        // Without the flag: 12 + 6 + 2*6 = 30 crossings at 6 s.
        assert_eq!(super::codebase_estimate_secs(&prepared, false), 180);
        // With it: another 6 s of cargo per crossing.
        assert_eq!(super::codebase_estimate_secs(&prepared, true), 360);

        let off = super::codebase_plan_line(&prepared, std::path::Path::new("/r"), false);
        assert!(!off.contains("exec"), "{off}");
        let on = super::codebase_plan_line(&prepared, std::path::Path::new("/r"), true);
        assert!(
            on.contains("+ exec: cold check unmeasured, then ~6 s per crossing"),
            "{on}"
        );
    }

    #[test]
    fn the_plan_line_names_every_tier_and_the_second_arm() {
        let line = super::codebase_plan_line(
            &prepared_counts(12, 6, 6),
            std::path::Path::new("/r"),
            false,
        );
        assert_eq!(
            line,
            "codebase: 24 tasks from /r @ 4818813deeaa (12 in_file, 6 function_body, \
             6 cross_file_first × 2 arms)\n",
            "{line}"
        );
        let none = super::codebase_plan_line(
            &prepared_counts(12, 12, 0),
            std::path::Path::new("/r"),
            false,
        );
        assert!(
            none.contains("(12 in_file, 12 function_body, 0 cross_file_first)"),
            "no second arm to announce: {none}"
        );
    }

    #[test]
    fn the_estimate_counts_a_cross_file_task_twice() {
        assert_eq!(
            super::codebase_estimate_secs(&prepared_counts(12, 6, 6), false),
            180
        );
        assert_eq!(
            super::codebase_estimate_secs(&prepared_counts(12, 12, 0), false),
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

    /// The canned registry the judge refusals read: `gpt-oss-20b` is the one
    /// entry carrying `role = "judge"`, and no shard exists on disk.
    fn judge_entry(name: &str) -> crate::core::registry::Effective {
        crate::core::registry::Effective {
            name: name.to_owned(),
            ctx_size: 4096,
            flags: vec![],
            entry: crate::core::registry::ModelEntry {
                repo: format!("unsloth/{name}-GGUF"),
                quant: "Q8_0".into(),
                revision: "d449b42d93e1c2c7bda5312f5c25c8fb91dfa9b4".into(),
                path: format!("models/{name}@d449b42d93e1"),
                first_shard: format!("{name}.gguf"),
                hermes_ok: true,
                ctx_size: None,
                extra_flags: vec![],
                role: (name == "gpt-oss-20b").then_some(crate::core::registry::ModelRole::Judge),
            },
        }
    }

    /// The two pure refusals of `resolve_judge` — the family check needs a
    /// real `GGUF` header and is covered by `judge::family_conflict`.
    fn judge_refusal(
        judge: Option<&str>,
        candidates: &[(&str, crate::core::bench::lifecycle::StepAction)],
    ) -> Option<ChekovError> {
        let name = judge?;
        let resolved: Vec<_> = candidates
            .iter()
            .map(|(n, action)| (judge_entry(n), *action))
            .collect();
        super::role_check(&judge_entry(name), name)
            .and_then(|()| super::server_check(&resolved))
            .err()
    }

    /// One run directory on disk holding the given codebase crossings.
    fn run_dir_with(
        name: &str,
        tiers: &[crate::core::bench::codebase::TaskTier],
    ) -> std::path::PathBuf {
        use crate::core::bench::store::{RunHead, RunWriter};
        let eval = std::env::temp_dir()
            .join("chekov-test-judge-eligible")
            .join(name);
        let _ = std::fs::remove_dir_all(&eval);
        std::fs::create_dir_all(&eval).expect("scratch dir");
        let head = RunHead {
            model: "ornith-1.5-35b-a3b".into(),
            machine_brand: None,
            launch_args: vec![],
            forced_reasoning_format: None,
            stamp: eligible_stamp(),
        };
        let mut writer = RunWriter::create(&eval, "r-eligible", &head).expect("create");
        for (n, tier) in tiers.iter().enumerate() {
            writer
                .append(crossing_task(&format!("t-{n}"), *tier))
                .expect("append");
        }
        writer.dir().to_path_buf()
    }

    fn eligible_stamp() -> crate::core::bench::stamp::Stamp {
        crate::core::bench::stamp::Stamp {
            machine_id: "8d41f0c2a917".into(),
            runtime: crate::core::bench::stamp::RUNTIME_LLAMA_CPP.to_owned(),
            timing_source: crate::core::bench::stamp::TIMING_SERVER.to_owned(),
            engine_build_commit: "dda1b0d67".into(),
            weights_revision: "fbbaed45c2f0/model-00001.gguf".into(),
            quant: "Q8_0".into(),
            ctx: 262_144,
            n_parallel: 1,
            kv_unified: "engine-default".into(),
            n_batch: "engine-default".into(),
            n_ubatch: "engine-default".into(),
            type_k: "q8_0".into(),
            type_v: "q8_0".into(),
            flash_attn: "on".into(),
            allow_exec: false,
            cargo_version: None,
            exec_target: "none".into(),
            seed: 42,
            temperature_milli: 0,
            chekov_version: "0.1.0".into(),
            prompt_set_hash: "e19a".into(),
            corpus_id: "codebase:4818813deeaa:abcdef123456".into(),
            judge: None,
        }
    }

    /// One answered codebase crossing — a prediction that differs from the
    /// gold, so a `function_body` one is a crossing the judge would be asked.
    fn crossing_row(
        tier: crate::core::bench::codebase::TaskTier,
    ) -> crate::core::bench::store::CodebaseRow {
        crate::core::bench::store::CodebaseRow {
            tier,
            file: "src/a.rs".into(),
            line: 7,
            label: "<mask>".into(),
            gold: "let a = 1;".into(),
            prediction: "let b = 2;".into(),
            prefix: "fn f() {\n".into(),
            suffix: "\n}\n".into(),
            excluded: crate::core::bench::codebase::Excluded::default(),
            symbols_score: Some(1.0),
            unsupported: false,
            arm: None,
            extra: None,
            also_first_uses: Vec::new(),
            name: None,
            n_predict: Some(64),
            exec: None,
        }
    }

    fn crossing_task(
        id: &str,
        tier: crate::core::bench::codebase::TaskTier,
    ) -> crate::core::bench::store::Task {
        crate::core::bench::store::Task {
            suite: "codebase".into(),
            task_id: id.to_owned(),
            measure: crate::core::bench::codebase::run::empty_measure(),
            grade: None,
            transport: crate::core::bench::store::Transport::Buffered,
            codebase: Some(crossing_row(tier)),
            judge: None,
        }
    }

    /// A judge is never loaded to record nothing: a run with no answered
    /// `function_body` crossing has nothing for it to be asked about, and
    /// neither has one whose crossings are all judged already.
    #[test]
    fn a_run_without_a_function_body_crossing_has_nothing_for_the_judge() {
        use crate::core::bench::codebase::TaskTier;
        let none = run_dir_with("in-file-only", &[TaskTier::InFile, TaskTier::InFile]);
        assert_eq!(super::eligible_crossings(&[none]).expect("load"), 0);
        let some = run_dir_with("one-body", &[TaskTier::InFile, TaskTier::FunctionBody]);
        assert_eq!(
            super::eligible_crossings(std::slice::from_ref(&some)).expect("load"),
            1
        );
        judge_the_crossings(&some);
        assert_eq!(
            super::eligible_crossings(&[some]).expect("load"),
            0,
            "a resumed run that owes no verdict loads no judge"
        );
    }

    /// Append a verdict for every `function_body` crossing the run holds —
    /// what a completed judge phase leaves behind.
    fn judge_the_crossings(dir: &std::path::Path) {
        use crate::core::bench::codebase::TaskTier;
        use crate::core::bench::store::{
            DecidedBy, JUDGE_SUITE, JudgeRow, RunLog, RunWriter, Task, Transport,
        };
        let log = RunLog::load(dir).expect("load");
        let eval = dir.parent().expect("eval dir");
        let run_id = dir.file_name().and_then(|n| n.to_str()).expect("run id");
        let (mut writer, _) = RunWriter::resume(eval, run_id, &log.head).expect("resume");
        for row in log.rows.iter().filter(|r| {
            r.codebase
                .as_ref()
                .is_some_and(|c| c.tier == TaskTier::FunctionBody)
        }) {
            writer
                .append(Task {
                    suite: JUDGE_SUITE.into(),
                    task_id: row.task_id.clone(),
                    measure: crate::core::bench::codebase::run::empty_measure(),
                    grade: None,
                    transport: Transport::Buffered,
                    codebase: None,
                    judge: Some(JudgeRow {
                        equivalent: Some(true),
                        gold_first: Some(true),
                        prediction_first: Some(true),
                        decided_by: DecidedBy::SwapAgreement,
                        skipped: None,
                        judge_secs: 2.0,
                    }),
                })
                .expect("append");
        }
    }

    /// The judge is a step but never a candidate: agreeing to "1 candidate(s)"
    /// must not quietly mean two model loads.
    #[test]
    fn the_confirm_prompt_names_the_judge_instead_of_counting_it_as_a_candidate() {
        use crate::core::bench::lifecycle::{BenchStep, StepAction};
        let step = |model: &str, action| BenchStep {
            model: model.to_owned(),
            action,
            weights_bytes: None,
        };
        let candidates = [step("ornith", StepAction::Launch)];
        assert_eq!(
            super::confirm_text(&candidates, 300),
            "bench 1 candidate(s) with launch + teardown, ~5 min estimated"
        );
        let with_judge = [
            step("ornith", StepAction::Launch),
            step("minimax", StepAction::Launch),
            step("gpt-oss-20b", StepAction::Judge),
        ];
        assert_eq!(
            super::confirm_text(&with_judge, 300),
            "bench 2 candidate(s) plus judge 'gpt-oss-20b' with launch + teardown, \
             ~5 min estimated"
        );
    }

    // --------------------------------------------------------- judge seam

    /// Pops one canned outcome per POST and counts what went out — the judge
    /// wire's only boundary (§8.2).
    struct CannedJudge {
        outcomes: std::cell::RefCell<std::collections::VecDeque<Result<String, ChekovError>>>,
        sent: std::cell::Cell<usize>,
    }

    impl CannedJudge {
        fn new(outcomes: Vec<Result<String, ChekovError>>) -> Self {
            Self {
                outcomes: std::cell::RefCell::new(outcomes.into()),
                sent: std::cell::Cell::new(0),
            }
        }
    }

    impl crate::core::hub::HttpClient for CannedJudge {
        fn get(&self, _url: &str) -> Result<String, ChekovError> {
            unreachable!("the judge wire never GETs")
        }

        fn post_json(&self, _req: &crate::core::hub::JsonRequest) -> Result<String, ChekovError> {
            self.sent.set(self.sent.get() + 1);
            self.outcomes
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Err(socket_down("no canned response left")))
        }
    }

    fn socket_down(reason: &str) -> ChekovError {
        ChekovError::EndpointDown {
            url: "http://fake".into(),
            reason: reason.to_owned(),
        }
    }

    /// A judge that ANSWERED, with a refusal.
    fn judge_refused() -> ChekovError {
        ChekovError::UpstreamRefused {
            url: "http://fake/v1/chat/completions".into(),
            status: 400,
            reason: "the grammar could not be compiled".into(),
        }
    }

    /// One llama-server reply carrying the judge's schema answer and the
    /// timings object every crossing is measured from.
    fn judge_reply(same_behavior: bool) -> String {
        serde_json::json!({
            "choices": [{
                "message": { "content": format!("{{\"same_behavior\":{same_behavior}}}") },
                "finish_reason": "stop",
            }],
            "usage": { "prompt_tokens": 900, "completion_tokens": 10 },
            "timings": {
                "cache_n": 0,
                "prompt_n": 900, "prompt_ms": 2000.0, "prompt_per_second": 450.0,
                "predicted_n": 10, "predicted_ms": 460.0, "predicted_per_second": 21.7
            }
        })
        .to_string()
    }

    fn judge_plan() -> crate::core::bench::judge::JudgePlan {
        crate::core::bench::judge::JudgePlan {
            judge: judge_entry("gpt-oss-20b"),
            arch: "gpt-oss".into(),
            rubric_hash: "9f8e7d6c5b4a".into(),
            max_tokens: 512,
            min_consistency_pct: 70,
            reasoning_effort: crate::core::config::ReasoningEffort::Low,
        }
    }

    /// What `verdict_for` decided, and how many requests that took.
    type Decided = (
        Result<Option<crate::core::bench::store::JudgeRow>, ChekovError>,
        usize,
    );

    /// `verdict_for` over a canned wire.
    fn verdict_over(
        outcomes: Vec<Result<String, ChekovError>>,
        row: &crate::core::bench::store::CodebaseRow,
    ) -> Decided {
        use crate::core::bench::runner::{ProbeWire, SamplingPins};
        let http = CannedJudge::new(outcomes);
        let facade = crate::core::proxy::claude::ClaudeFacade::new("gpt-oss-20b");
        let upstream = crate::core::proxy::serve::Upstream {
            base_url: "http://fake".into(),
            api_key: "sekrit".into(),
        };
        let wire = ProbeWire {
            http: &http,
            facade: &facade,
            upstream: &upstream,
            pins: SamplingPins { seed: 42 },
        };
        let out = super::verdict_for(&wire, row, &judge_plan());
        (out, http.sent.get())
    }

    fn body_crossing() -> crate::core::bench::store::CodebaseRow {
        crossing_row(crate::core::bench::codebase::TaskTier::FunctionBody)
    }

    /// Both orders answered the same thing: one verdict, the raw answers kept
    /// beside it, and exactly two requests spent on it.
    #[test]
    fn two_agreeing_orders_are_one_verdict_and_cost_exactly_two_requests() {
        use crate::core::bench::store::DecidedBy;
        let (out, sent) = verdict_over(
            vec![Ok(judge_reply(true)), Ok(judge_reply(true))],
            &body_crossing(),
        );
        let row = out.expect("a verdict").expect("a judge row");
        assert_eq!(
            (row.equivalent, row.gold_first, row.prediction_first),
            (Some(true), Some(true), Some(true))
        );
        assert_eq!(row.decided_by, DecidedBy::SwapAgreement);
        assert_eq!(sent, 2, "one request per order, and no more");
    }

    /// A judge that ANSWERS a non-2xx is up and reachable: its refusal skips
    /// this crossing and the phase goes on. A judge that is not answering at
    /// all stops the phase with the rows so far intact.
    #[test]
    fn a_refusal_skips_the_crossing_and_a_dead_socket_stops_the_phase() {
        use crate::core::bench::store::DecidedBy;
        let (out, _) = verdict_over(
            vec![Err(judge_refused()), Err(judge_refused())],
            &body_crossing(),
        );
        let row = out
            .expect("a refusal is not the phase's error")
            .expect("a judge row");
        assert_eq!(row.decided_by, DecidedBy::Skipped);
        assert!(
            row.skipped
                .as_deref()
                .is_some_and(|s| s.starts_with("judge refused: ")),
            "{:?}",
            row.skipped
        );
        let (out, _) = verdict_over(
            vec![Err(socket_down("connection refused"))],
            &body_crossing(),
        );
        assert!(matches!(out, Err(ChekovError::EndpointDown { .. })));
    }

    /// The three crossings the judge is never asked about: another tier, an
    /// identical pair, and one the compiler already rejected. None of them
    /// reaches the wire, and none of them records time it did not spend.
    #[test]
    fn a_settled_crossing_records_its_row_without_a_request() {
        use crate::core::bench::codebase::TaskTier;
        use crate::core::bench::store::{DecidedBy, ExecRow, ExecScore};
        let (out, sent) = verdict_over(vec![], &crossing_row(TaskTier::InFile));
        assert!(out.expect("not an error").is_none(), "not a judge row");
        assert_eq!(sent, 0);

        let mut identical = body_crossing();
        identical.prediction.clone_from(&identical.gold);
        let (out, sent) = verdict_over(vec![], &identical);
        let row = out.expect("a row").expect("a judge row");
        assert_eq!(
            (row.decided_by, row.equivalent),
            (DecidedBy::Identical, Some(true))
        );
        assert_eq!(sent, 0);
        assert!(row.judge_secs.abs() < f64::EPSILON, "{}", row.judge_secs);

        let mut broken = body_crossing();
        broken.exec = Some(ExecRow {
            compile: ExecScore::Value(0.0),
            ..ExecRow::skipped("")
        });
        let (out, sent) = verdict_over(vec![], &broken);
        let row = out.expect("a row").expect("a judge row");
        assert_eq!(row.decided_by, DecidedBy::Skipped);
        assert_eq!(row.skipped.as_deref(), Some("did not compile"));
        assert_eq!(sent, 0);
        assert!(row.judge_secs.abs() < f64::EPSILON, "{}", row.judge_secs);
    }

    #[test]
    fn judge_without_a_role_and_a_reused_server_are_refused_before_any_launch() {
        use crate::core::bench::lifecycle::StepAction;
        let err = judge_refusal(Some("plain"), &[("ornith", StepAction::Launch)]);
        assert!(matches!(err, Some(ChekovError::JudgeNoRole { .. })));
        let err = judge_refusal(Some("gpt-oss-20b"), &[("ornith", StepAction::UseRunning)]);
        assert!(matches!(err, Some(ChekovError::JudgeNeedsTheServer)));
        assert!(judge_refusal(None, &[("ornith", StepAction::Launch)]).is_none());
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
