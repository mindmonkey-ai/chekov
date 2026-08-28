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
}

impl<'a> From<&'a BenchOpts> for BenchArgs<'a> {
    fn from(opts: &'a BenchOpts) -> Self {
        Self {
            fixture: opts.fixture.as_deref(),
            resume: opts.resume.as_deref(),
            models: &opts.models,
            dry_run: opts.dry_run,
            yes: opts.yes,
        }
    }
}

fn bench(ctx: &Ctx, args: &BenchArgs) -> Result<ExitCode, ChekovError> {
    use crate::core::bench::{lifecycle, sweep};
    let candidates = resolve_candidates(ctx, args)?;
    let plan: sweep::SweepPlan = (&ctx.config.file.bench).into();
    let steps: Vec<lifecycle::BenchStep> = candidates
        .iter()
        .map(|(eff, action)| lifecycle::BenchStep {
            model: eff.name.clone(),
            action: *action,
            weights_bytes: weights_on_disk(ctx, &eff.entry),
        })
        .collect();
    let estimate = lifecycle::estimate_secs(&steps, &plan);
    if args.dry_run {
        print!("{}", lifecycle::render_plan(&steps, estimate));
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
        let dir = run_candidate(ctx, candidate, args)?;
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
    args: &BenchArgs,
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
    let dir = measure_candidate(ctx, &setup, args)?;
    if *action == StepAction::Launch {
        teardown_candidate(ctx, pid)?;
    }
    Ok(dir)
}

/// Readiness through rendering for one already-up server.
fn measure_candidate(
    ctx: &Ctx,
    setup: &BenchSetup,
    args: &BenchArgs,
) -> Result<std::path::PathBuf, ChekovError> {
    use crate::core::bench::{store, sweep};
    use crate::core::proxy::serve::Upstream;
    let cfg = &ctx.config;
    let upstream = Upstream {
        base_url: cfg.base_url(),
        api_key: cfg.file.server.api_key.clone(),
    };
    let props = ensure_ready(ctx, &upstream, setup)?;
    let plan: sweep::SweepPlan = (&cfg.file.bench).into();
    let head = build_head(
        ctx,
        setup,
        &HeadInputs {
            props,
            plan: &plan,
            fixture: args.fixture,
        },
    )?;
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
}

fn run_suites(sink: &mut TaskSink, ctx: &Ctx, inputs: &SuiteInputs) -> Result<(), ChekovError> {
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
    run_throughput(sink, inputs.plan, &wire)?;
    if let Some(path) = inputs.fixture {
        run_fixture(sink, &wire, path)?;
    }
    Ok(())
}

/// Create a fresh run, or reopen one for `--resume` (stamp must match).
fn open_run(
    ctx: &Ctx,
    head: &crate::core::bench::store::RunHead,
    resume: Option<&str>,
) -> Result<(crate::core::bench::store::RunWriter, Vec<(String, String)>), ChekovError> {
    use crate::core::bench::store::RunWriter;
    let eval = ctx.config.eval_dir();
    if let Some(run_id) = resume {
        let (writer, log) = RunWriter::resume(&eval, run_id, head)?;
        let done = log
            .rows
            .iter()
            .map(|r| (r.suite.clone(), r.task_id.clone()))
            .collect();
        return Ok((writer, done));
    }
    let run_id = format!("{}-{}", crate::core::clock::utc_compact_now(), head.model);
    Ok((RunWriter::create(&eval, &run_id, head)?, Vec::new()))
}

/// Where task rows land, plus what a resumed run already holds.
struct TaskSink<'a> {
    writer: &'a mut crate::core::bench::store::RunWriter,
    done: &'a [(String, String)],
}

impl TaskSink<'_> {
    fn is_done(&self, suite: &str, task_id: &str) -> bool {
        self.done.iter().any(|(s, t)| s == suite && t == task_id)
    }
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
        if sink.is_done("throughput", &task_id) {
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
    use crate::core::bench::{fixture, grade, probes, runner, store};
    let loaded = fixture::load(path)?;
    for probe in &loaded.probes {
        if sink.is_done("fixture", &probe.id) {
            eprintln!(
                "chekov: fixture {} already recorded — skipped (--resume)",
                probe.id
            );
            continue;
        }
        let outcome = runner::cross(wire, &probes::fixture_probe(probe)).map(|artifact| {
            (
                artifact.timings,
                grade::grade(&artifact.anthropic_body, probe),
            )
        });
        let (measure, verdict) = match outcome {
            Ok((timings, graded)) => (probe_measure(&timings), grade_row(graded)),
            Err(e) => failed_probe(&e),
        };
        sink.writer.append(store::Task {
            suite: "fixture".into(),
            task_id: probe.id.clone(),
            measure,
            grade: Some(verdict),
        })?;
    }
    Ok(())
}

/// A crossing failure is a FAILED probe with the error as its reason and no
/// invented measurement — never an empty reply.
fn failed_probe(
    e: &ChekovError,
) -> (
    crate::core::bench::store::Measure,
    crate::core::bench::store::GradeRow,
) {
    use crate::core::bench::store;
    (
        store::Measure {
            prompt_n: 0,
            decode_samples: vec![],
            prefill_samples: vec![],
            warmup_dropped: 0,
            cache_n: 0,
        },
        store::GradeRow {
            pass: false,
            reason: Some(e.to_string()),
        },
    )
}

fn probe_measure(
    timings: &crate::core::bench::runner::Timings,
) -> crate::core::bench::store::Measure {
    crate::core::bench::store::Measure {
        prompt_n: timings.prompt_n,
        decode_samples: vec![timings.predicted_per_second],
        prefill_samples: vec![timings.prompt_per_second],
        warmup_dropped: 0,
        cache_n: timings.cache_n,
    }
}

fn grade_row(graded: crate::core::bench::grade::Grade) -> crate::core::bench::store::GradeRow {
    use crate::core::bench::{grade, store};
    match graded {
        grade::Grade::Pass => store::GradeRow {
            pass: true,
            reason: None,
        },
        grade::Grade::Fail { reason } => store::GradeRow {
            pass: false,
            reason: Some(reason),
        },
    }
}

/// Everything the stamp is built from beyond `Ctx` and the setup (§4).
struct HeadInputs<'a> {
    props: crate::core::bench::runner::PropsInfo,
    plan: &'a crate::core::bench::sweep::SweepPlan,
    fixture: Option<&'a std::path::Path>,
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

fn build_head(
    ctx: &Ctx,
    setup: &BenchSetup,
    inputs: &HeadInputs,
) -> Result<crate::core::bench::store::RunHead, ChekovError> {
    use crate::core::bench::{probes, stamp, store};
    let cfg = &ctx.config;
    let (machine_id, machine_brand, engine) = stamp_identity(cfg)?;
    let launch_args = crate::core::server::launch_args(cfg, &setup.eff);
    let bench_cfg = &cfg.file.bench;
    let flags = stamped_flags(&launch_args);
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
        prompt_set_hash: probes::prompt_set_hash(inputs.plan, bench_cfg.seed),
        corpus_id: corpus_id(inputs.fixture)?,
    };
    Ok(store::RunHead {
        model: setup.eff.name.clone(),
        machine_brand,
        launch_args,
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

/// "throughput-v1", plus the fixture file's content hash when one is graded —
/// runs over different task sets must never compare as the same set.
fn corpus_id(fixture: Option<&std::path::Path>) -> Result<String, ChekovError> {
    let Some(path) = fixture else {
        return Ok("throughput-v1".to_owned());
    };
    let text = std::fs::read_to_string(path).map_err(|e| ChekovError::FixtureInvalid {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    let digest = crate::core::hash::sha256_hex(text.as_bytes());
    Ok(format!("throughput-v1+fixture:{}", &digest[..12]))
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
