//! `chekov tune [NAME]` — measure which launch flags beat the current ones on
//! this machine, and say so honestly (spec §2 surface, §7 plan, §9 report).
//!
//! The descent itself is `core::tune`'s: this module owns the plan a human
//! agrees to, the four-stage loop through the shared candidate lifecycle, and
//! the report the run ends with.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::{Command, Ctx, confirm};
use crate::core::bench::sweep::SweepPlan;
use crate::core::bench::{candidate, lifecycle, runner, sweep};
use crate::core::config::TuneSection;
use crate::core::proxy::claude::ClaudeFacade;
use crate::core::proxy::serve::Upstream;
use crate::core::registry::Effective;
use crate::core::stats::{Comparison, Summary, compare};
use crate::core::tune::{
    self, CandidateLabel, JudgeCriteria, LineContext, Measured, Metric, Outcome, Probe, Record,
    Stage, Trial, Verdict,
};
use crate::core::{footprint, machine, server};
use crate::error::ChekovError;

/// `chekov tune` — the four-stage descent over launch flags (spec §2).
#[derive(Debug, clap::Args)]
pub struct TuneCmd {
    /// Model to tune (defaults to the active model).
    pub name: Option<String>,
    /// Print the stage plan, the launch count and the estimate; launch nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Pre-approve the confirm gate (every trial is a launch).
    #[arg(long)]
    pub yes: bool,
    /// Write the winning flags into the model's `extra_flags`.
    #[arg(long)]
    pub apply: bool,
    /// Restrict the descent to these stages: fa, kv, batch, ubatch.
    #[arg(long, value_delimiter = ',')]
    pub stages: Vec<String>,
}

/// What one run measures: the model as it launches today, the stages to walk,
/// and the probe every trial is measured with.
struct Plan<'a> {
    eff: Effective,
    stages: Vec<Stage>,
    tune: &'a TuneSection,
    sweep: SweepPlan,
    significance_pct: f64,
}

/// The configuration later candidates are judged against — the baseline, then
/// whatever has won so far (spec §4).
struct Incumbent {
    argv: Vec<String>,
    measured: Measured,
}

/// One completed trial: which candidate (none for the baseline), the flags it
/// ran, what came back, and the clock before and after the probe.
struct TrialOutcome {
    picked: Option<tune::Candidate>,
    argv: Vec<String>,
    outcome: Outcome,
    therm: [Option<u32>; 2],
}

/// Whether a trial reached a spawn, or was skipped before one.
enum Started {
    Pid(i32),
    Skipped(String),
}

/// A finished trial beside the verdict of the teardown that followed it.
///
/// The two travel together so the caller can record the trial BEFORE it
/// propagates a teardown failure: `BenchBudgetNotReleased` is fatal (spec §11
/// — the next trial would measure a contended machine), but a measurement
/// already taken must not be thrown away with it.
struct Completed {
    trial: TrialOutcome,
    released: Result<(), ChekovError>,
}

/// The run's verdict when the final incumbent is not the baseline (spec §8).
const CANDIDATE_WON: &str = "a candidate beat the current flags";

/// Spec §9's `defaults won` line. The threshold is the record's own — the one
/// every verdict in that run was reached under — never a restated default.
fn defaults_won_line(significance_pct: f64) -> String {
    format!(
        "{} — no candidate beat the current flags at p < {significance_pct:.0}% on its metric",
        tune::DEFAULTS_WON
    )
}

/// llama-server's own value for each stage's flag when the flag is absent
/// (`llama-server --help`). An absent flag still names a configuration, so a
/// candidate carrying that value IS the incumbent, not another launch.
///
/// `Batch` defers to `tune::ENGINE_DEFAULT_BATCH` — the same fact
/// `incumbent_batch` filters the ubatch list against — so the two modules
/// cannot drift apart on what llama-server actually defaults to.
fn engine_default(stage: Stage) -> String {
    match stage {
        Stage::Fa => "auto".to_owned(),
        Stage::Kv => "f16".to_owned(),
        Stage::Batch => tune::ENGINE_DEFAULT_BATCH.to_string(),
        Stage::Ubatch => "512".to_owned(),
    }
}

/// The flag a stage's value is read from. `Kv` writes K and V together and
/// reads K — they never diverge, because `candidates` rewrites both.
const fn flag_of(stage: Stage) -> tune::Flag {
    match stage {
        Stage::Fa => tune::Flag::FlashAttn,
        Stage::Kv => tune::Flag::CacheTypeK,
        Stage::Batch => tune::Flag::BatchSize,
        Stage::Ubatch => tune::Flag::UbatchSize,
    }
}

/// The value the incumbent already runs for `stage`'s flag.
fn incumbent_value(stage: Stage, incumbent: &[String]) -> String {
    tune::value_of(incumbent, flag_of(stage)).unwrap_or_else(|| engine_default(stage))
}

/// The candidates a stage actually launches: its value list minus the one the
/// incumbent already runs. Rewriting a flag to the value it already carries —
/// explicitly, or by the engine's default — would measure the incumbent twice.
fn planned(stage: Stage, incumbent: &[String], cfg: &TuneSection) -> Vec<tune::Candidate> {
    let current = incumbent_value(stage, incumbent);
    tune::candidates(stage, incumbent, cfg)
        .into_iter()
        .filter(|candidate| candidate.value != current)
        .collect()
}

/// The largest `--batch-size` this descent can end up at: any value the batch
/// stage may win, or the baseline's own when nothing configured beats it.
fn widest_batch(plan: &Plan) -> u32 {
    let current = incumbent_value(Stage::Batch, &plan.eff.flags)
        .parse()
        .unwrap_or(0);
    plan.tune
        .batch_sizes
        .iter()
        .copied()
        .chain([current])
        .max()
        .unwrap_or(current)
}

/// The argv a stage is COUNTED against in the plan. Only `ubatch` differs from
/// the baseline: its list is filtered by the incumbent batch, so a batch stage
/// that wins a value above the baseline's GROWS it. Counting `ubatch` against
/// the widest batch the descent can reach is what makes the printed `≤ N` a
/// real ceiling rather than a number the run can quietly exceed (spec §7).
fn counted_against(plan: &Plan, stage: Stage) -> Vec<String> {
    if stage != Stage::Ubatch || !plan.stages.contains(&Stage::Batch) {
        return plan.eff.flags.clone();
    }
    let widest = widest_batch(plan).to_string();
    tune::rewrite(&plan.eff.flags, tune::Flag::BatchSize, &widest)
}

/// The launch ceiling: the baseline plus one launch per non-incumbent
/// candidate. An upper bound for every descent outcome — a stage's winner can
/// only shrink what follows, and a skip costs nothing (spec §7).
fn max_launches(plan: &Plan) -> usize {
    1 + plan
        .stages
        .iter()
        .map(|&stage| planned(stage, &counted_against(plan, stage), plan.tune).len())
        .sum::<usize>()
}

/// One stage's plan line: how many values it lists, and what the parenthetical
/// has to say about them (spec §7).
fn stage_plan_line(plan: &Plan, stage: Stage) -> String {
    let argv = counted_against(plan, stage);
    let listed = tune::values_for(stage, &argv, plan.tune).len();
    let held = if listed > planned(stage, &argv, plan.tune).len() {
        "1 is the incumbent"
    } else {
        "none is the incumbent"
    };
    let note = match stage {
        Stage::Kv => "; the f16 KV is footprint-gated per trial",
        Stage::Ubatch => "; values ≤ the incumbent batch",
        Stage::Fa | Stage::Batch => "",
    };
    let label = stage.label();
    format!("  {label:<10} {listed} candidates   ({held}{note})\n")
}

/// One probe's wall clock at `lifecycle`'s reference rates (spec §7).
fn probe_secs(plan: &Plan) -> u64 {
    let per_rep = u64::from(plan.tune.depth) * 1000 / lifecycle::PREFILL_TOK_S
        + u64::from(plan.sweep.max_tokens) * 1000 / lifecycle::DECODE_TOK_S;
    (u64::from(plan.sweep.repetitions) * per_rep).div_ceil(1000)
}

/// Every launch's load plus every launch's probe. An estimate, never a promise.
fn estimate_secs(plan: &Plan, weights_bytes: Option<u64>) -> u64 {
    let launches = u64::try_from(max_launches(plan)).unwrap_or(u64::MAX);
    let load = weights_bytes.unwrap_or(0) / lifecycle::GIB * lifecycle::LOAD_MS_PER_GIB / 1000;
    launches * (load + probe_secs(plan))
}

/// The plan's last line, which is also the confirm prompt (spec §7).
fn estimate_line(plan: &Plan, weights_bytes: Option<u64>) -> String {
    let launches = max_launches(plan);
    let minutes = estimate_secs(plan, weights_bytes).div_ceil(60);
    let rate = lifecycle::LOAD_MS_PER_GIB / 1000;
    let weights = weights_bytes.map_or_else(|| "? GiB".to_owned(), lifecycle::render_gib);
    let probe = probe_secs(plan);
    format!(
        "  ≤ {launches} launches, ~{minutes} min estimated \
         (load ≈ {rate} s/GiB × {weights}, probe ≈ {probe} s each)\n"
    )
}

/// Spec §7's plan: what will be measured, how many launches at most, and how
/// long that is likely to take. `--dry-run` prints exactly this and stops.
fn plan_text(plan: &Plan, weights_bytes: Option<u64>, running: Option<&str>) -> String {
    let flags = plan.eff.flags.join(" ");
    let (name, ctx) = (&plan.eff.name, plan.eff.ctx_size);
    let (depth, reps) = (plan.tune.depth, plan.sweep.repetitions);
    let mut parts = vec![
        format!("tune {name} @ ctx {ctx}, probe depth {depth} × {reps} reps\n"),
        format!("  {:<10} {flags}   (current flags)\n", "baseline"),
    ];
    parts.extend(running.map(|running| {
        format!(
            "  {:<10} will stop the running '{running}' first — it is not restarted\n",
            "server"
        )
    }));
    parts.extend(
        plan.stages
            .iter()
            .map(|&stage| stage_plan_line(plan, stage)),
    );
    parts.push(estimate_line(plan, weights_bytes));
    parts.concat()
}

/// The confirm gate's prompt is the plan's last line (spec §7).
fn prompt_of(plan_text: &str) -> &str {
    plan_text.lines().last().unwrap_or("tune").trim()
}

/// `need − budget`, in the words spec §2 gives a trial that cannot be launched.
fn over_budget(need_mib: u64, budget_mib: u64) -> String {
    let over = need_mib.saturating_sub(budget_mib);
    format!("exceeds the GPU budget by {over} MiB")
}

/// The one launch refusal that is a skipped trial rather than a failed run: a
/// wider KV cache is a legitimate candidate this machine cannot hold. Every
/// other refusal (port, shard, a foreign server) stops the run (spec §2).
fn skip_reason(err: &ChekovError) -> Option<String> {
    match err {
        ChekovError::ModelExceedsBudget {
            need_mib,
            budget_mib,
            ..
        } => Some(over_budget(*need_mib, *budget_mib)),
        _ => None,
    }
}

/// `q8_0` KV needs flash attention on; under an FA-off incumbent that trial is
/// skipped before any spawn (spec §4).
fn kv_skip(candidate: &tune::Candidate, incumbent: &[String]) -> Option<String> {
    let fa_off = tune::value_of(incumbent, tune::Flag::FlashAttn).as_deref() == Some("off");
    (candidate.stage == Stage::Kv && candidate.value == "q8_0" && fa_off)
        .then(|| "q8_0 KV needs flash attention on".to_owned())
}

/// True for any V-cache spelling llama.cpp will refuse to pair with `fa
/// off` — everything except absent/`f16`/`bf16`/`f32` (spec §4 mirror).
fn quantized_v_cache(incumbent: &[String]) -> Option<String> {
    let v_cache = tune::value_of(incumbent, tune::Flag::CacheTypeV)?;
    (!matches!(v_cache.as_str(), "f16" | "bf16" | "f32")).then_some(v_cache)
}

/// The mirror of `kv_skip`: `fa off` needs unquantized KV, and llama.cpp
/// exits at load naming the V cache when it is not — so under a quantized
/// incumbent that trial is skipped before any spawn (evidence 2026-09-01,
/// IDEAS.md; spec §4).
fn fa_skip(candidate: &tune::Candidate, incumbent: &[String]) -> Option<String> {
    let v_cache = quantized_v_cache(incumbent)?;
    (candidate.stage == Stage::Fa && candidate.value == "off").then(|| {
        format!(
            "fa off requires unquantized KV — llama.cpp refuses the combination; \
             skipped under a {v_cache} incumbent"
        )
    })
}

/// Who measured, and under what probe (spec §9's first line).
fn header(record: &Record) -> String {
    let Record {
        model,
        quant,
        revision,
        machine_id,
        engine_build_commit,
        probe,
        ..
    } = record;
    let (depth, reps) = (probe.depth, probe.repetitions);
    format!(
        "tune {model} ({quant}@{revision}) — machine {machine_id}, \
         engine {engine_build_commit}, probe depth {depth} × {reps}\n"
    )
}

/// A recorded trial's two summaries, when it carries both.
fn measured_of(trial: &Trial) -> Option<Measured> {
    Some(Measured {
        decode: trial.decode.clone()?,
        prefill: trial.prefill.clone()?,
        prompt_n: trial.prompt_n?,
    })
}

fn baseline_line(trial: &Trial) -> String {
    let cells = measured_of(trial).map_or_else(String::new, |m| tune::measured_cells(&m));
    let flags = trial.argv.join(" ");
    format!("  {:<10} {cells}   {flags}\n", "baseline")
}

/// The same rule the run's own verdicts were reached under (`stats::compare`
/// at the record's threshold), so the report never calls a difference
/// significant that the descent did not. A separated pair prints its signed
/// median delta and claims nothing more.
fn delta_note(pair: (&Summary, &Summary), significance_pct: f64) -> String {
    let (candidate, baseline) = pair;
    if baseline.median <= 0.0 {
        return "no baseline median".to_owned();
    }
    if compare(candidate, baseline, significance_pct) == Comparison::NoSignificantDifference {
        return "no significant difference".to_owned();
    }
    let pct = (candidate.median - baseline.median) / baseline.median * 100.0;
    format!("{pct:+.0}%")
}

/// One metric's winner-versus-baseline cell (spec §9).
fn versus(metric: Metric, pair: (&Summary, &Summary), significance_pct: f64) -> String {
    let note = delta_note(pair, significance_pct);
    let (won, base) = (pair.0.median, pair.1.median);
    match metric {
        Metric::Decode => format!("decode {won:.1} vs {base:.1} ({note})"),
        Metric::Prefill => format!("prefill {won:.0} vs {base:.0} ({note})"),
    }
}

/// The winner's numbers beside the baseline's, on the line under the winner.
fn winner_versus(record: &Record, winner: &[String]) -> Option<String> {
    let baseline = measured_of(record.trials.first()?)?;
    let won = measured_of(record.trials.iter().rev().find(|t| t.argv == winner)?)?;
    let pct = record.significance_pct;
    let decode = versus(Metric::Decode, (&won.decode, &baseline.decode), pct);
    let prefill = versus(Metric::Prefill, (&won.prefill, &baseline.prefill), pct);
    Some(format!("  {:<10} {decode}   {prefill}\n", ""))
}

/// The winner block, or the one line that says nothing beat the baseline.
fn verdict_block(record: &Record) -> String {
    let Some(winner) = record.winner.as_deref() else {
        return format!("  {}\n", defaults_won_line(record.significance_pct));
    };
    let flags = winner.join(" ");
    let named = format!("  {:<10} {flags}\n", "winner");
    named + &winner_versus(record, winner).unwrap_or_default()
}

/// Spec §9's report: the header, the baseline, every stage line, the verdict,
/// where the record went, and — with a winner — how to apply it.
fn report(record: &Record, lines: &[String], path: &Path) -> String {
    let written = path.display();
    let mut parts = vec![header(record)];
    parts.extend(record.trials.first().map(baseline_line));
    parts.extend(lines.iter().map(|line| format!("{line}\n")));
    parts.push(verdict_block(record));
    parts.push(format!("  {:<10} {written}\n", "record"));
    if record.winner.is_some() {
        let model = &record.model;
        parts.push(format!(
            "  apply with: chekov tune {model} --apply   \
             (or add the flags to extra_flags by hand)\n"
        ));
    }
    parts.concat()
}

/// The record row for one trial (spec §8's shape).
fn trial_row(trial: &TrialOutcome, verdict: Option<&Verdict>) -> Trial {
    let measured = match &trial.outcome {
        Outcome::Measured(measured) => Some(measured),
        Outcome::Degenerate(_) | Outcome::Skipped(_) => None,
    };
    let (outcome, reason) = outcome_words(&trial.outcome);
    Trial {
        stage: trial
            .picked
            .as_ref()
            .map_or("baseline", |p| p.stage.label())
            .to_owned(),
        value: trial.picked.as_ref().map(|p| p.value.clone()),
        stamp: tune::sextet(&trial.argv),
        argv: trial.argv.clone(),
        outcome: outcome.to_owned(),
        decode: measured.map(|m| m.decode.clone()),
        prefill: measured.map(|m| m.prefill.clone()),
        prompt_n: measured.map(|m| m.prompt_n),
        speed_limit_pct: trial.therm,
        reason,
        verdict: verdict.map(|v| v.phrase.clone()),
    }
}

/// The record's word for an outcome, and the reason that goes with it.
fn outcome_words(outcome: &Outcome) -> (&'static str, Option<String>) {
    match outcome {
        Outcome::Measured(_) => ("measured", None),
        Outcome::Degenerate(reason) => ("degenerate", Some(reason.clone())),
        Outcome::Skipped(reason) => ("skipped", Some(reason.clone())),
    }
}

/// `Measured` carries no `Clone`; the winner's numbers are copied field-wise
/// so the next stage's incumbent owns them.
fn carried(measured: &Measured) -> Measured {
    Measured {
        decode: measured.decode.clone(),
        prefill: measured.prefill.clone(),
        prompt_n: measured.prompt_n,
    }
}

/// The server chekov's own pidfile names, if one is up. A live server whose
/// run state cannot be read is `ServerModelUnknown` — tune only ever stops a
/// server it can name (spec §2).
fn running_server(ctx: &Ctx) -> Result<Option<(i32, String)>, ChekovError> {
    let Some(pid) = server::live_pid(&ctx.config) else {
        return Ok(None);
    };
    let name = server::read_run_state(&ctx.config).ok_or(ChekovError::ServerModelUnknown)?;
    Ok(Some((pid, name)))
}

/// One run's mutable state: where the record goes, what it holds so far, and
/// the report line each trial produced.
struct Session<'a> {
    ctx: &'a Ctx,
    plan: &'a Plan<'a>,
    path: PathBuf,
    record: Record,
    lines: Vec<String>,
}

impl<'a> Session<'a> {
    fn new(ctx: &'a Ctx, plan: &'a Plan<'a>) -> Result<Self, ChekovError> {
        let (machine_id, _, engine_build_commit) = super::capability::stamp_identity(&ctx.config)?;
        let eff = &plan.eff;
        Ok(Self {
            ctx,
            plan,
            path: tune::record_path(&ctx.config.tune_dir(), &eff.name),
            record: Record {
                model: eff.name.clone(),
                quant: eff.entry.quant.clone(),
                revision: eff.entry.revision.clone(),
                machine_id,
                engine_build_commit,
                chekov_version: env!("CARGO_PKG_VERSION").to_owned(),
                probe: Probe {
                    depth: plan.tune.depth,
                    repetitions: plan.sweep.repetitions,
                    max_tokens: plan.sweep.max_tokens,
                },
                significance_pct: plan.significance_pct,
                thermal_source: tune::THERMAL_SOURCE.to_owned(),
                trials: Vec::new(),
                winner: None,
                verdict: tune::DEFAULTS_WON.to_owned(),
            },
            lines: Vec::new(),
        })
    }

    /// The baseline, then each stage against whatever has won so far (spec §4).
    fn descend(&mut self) -> Result<(), ChekovError> {
        let mut incumbent = self.baseline()?;
        let stages = self.plan.stages.clone();
        for stage in stages {
            if let Some(winner) = self.stage(stage, &incumbent)? {
                incumbent = winner;
            }
        }
        self.settle(&incumbent.argv)
    }

    /// Trial 0: the model's current flags, unchanged. A degenerate baseline
    /// ends the run — there is nothing to compare against (spec §5).
    fn baseline(&mut self) -> Result<Incumbent, ChekovError> {
        let Completed { trial, released } = self.measure(None)?;
        self.append(&trial, None)?;
        released?;
        let argv = trial.argv;
        match trial.outcome {
            Outcome::Measured(measured) => Ok(Incumbent { argv, measured }),
            Outcome::Degenerate(reason) | Outcome::Skipped(reason) => {
                Err(ChekovError::TuneBaselineDegenerate {
                    name: self.plan.eff.name.clone(),
                    reason,
                })
            }
        }
    }

    /// Every candidate of one stage, recorded as it completes; the stage's
    /// winner, if any, becomes the next stage's incumbent (spec §4).
    fn stage(
        &mut self,
        stage: Stage,
        incumbent: &Incumbent,
    ) -> Result<Option<Incumbent>, ChekovError> {
        let mut scored = Vec::new();
        for candidate in planned(stage, &incumbent.argv, self.plan.tune) {
            let Completed { trial, released } = self.trial(candidate, incumbent)?;
            let verdict = self.verdict_for(&trial, incumbent);
            self.append(&trial, verdict.as_ref())?;
            released?;
            if let (Outcome::Measured(m), Some(v), Some(p)) = (trial.outcome, verdict, trial.picked)
            {
                scored.push((p, m, v));
            }
        }
        Ok(
            tune::pick_winner(&scored).map(|(picked, measured, _)| Incumbent {
                argv: picked.argv.clone(),
                measured: carried(measured),
            }),
        )
    }

    /// One candidate's trial, or the pre-launch skip spec §4 names.
    fn trial(
        &self,
        candidate: tune::Candidate,
        incumbent: &Incumbent,
    ) -> Result<Completed, ChekovError> {
        match kv_skip(&candidate, &incumbent.argv).or_else(|| fa_skip(&candidate, &incumbent.argv))
        {
            Some(reason) => Ok(Completed {
                trial: TrialOutcome {
                    argv: candidate.argv.clone(),
                    picked: Some(candidate),
                    outcome: Outcome::Skipped(reason),
                    therm: [None, None],
                },
                released: Ok(()),
            }),
            None => self.measure(Some(candidate)),
        }
    }

    /// A measured candidate judged against the incumbent on its stage's metric.
    fn verdict_for(&self, trial: &TrialOutcome, incumbent: &Incumbent) -> Option<Verdict> {
        let Outcome::Measured(measured) = &trial.outcome else {
            return None;
        };
        let stage = trial.picked.as_ref()?.stage;
        Some(tune::judge(
            measured,
            &incumbent.measured,
            JudgeCriteria {
                stage,
                significance_pct: self.plan.significance_pct,
            },
        ))
    }

    /// Launch, wait, probe, tear down — always tearing down what was launched
    /// (spec §3). `None` measures the baseline's own flags. The teardown's
    /// verdict rides back beside the trial rather than short-circuiting it:
    /// the caller records the measurement first, then propagates.
    fn measure(&self, picked: Option<tune::Candidate>) -> Result<Completed, ChekovError> {
        let argv = picked
            .as_ref()
            .map_or_else(|| self.plan.eff.flags.clone(), |p| p.argv.clone());
        let eff = Effective {
            flags: argv.clone(),
            ..self.plan.eff.clone()
        };
        let pid = match self.start(&eff)? {
            Started::Skipped(reason) => {
                let outcome = Outcome::Skipped(reason);
                let trial = TrialOutcome {
                    picked,
                    argv,
                    outcome,
                    therm: [None, None],
                };
                return Ok(Completed {
                    trial,
                    released: Ok(()),
                });
            }
            Started::Pid(pid) => pid,
        };
        let (outcome, therm) = self.probe(&candidate::Candidate { eff, pid });
        let released = candidate::teardown(self.ctx, pid);
        let trial = TrialOutcome {
            picked,
            argv,
            outcome,
            therm,
        };
        Ok(Completed { trial, released })
    }

    /// The footprint gate before the spawn, then `launch`'s own preflight —
    /// a doomed trial costs nothing, and a late refusal is still a skip.
    fn start(&self, eff: &Effective) -> Result<Started, ChekovError> {
        if let Some(reason) = self.budget_skip(eff) {
            return Ok(Started::Skipped(reason));
        }
        match candidate::launch(self.ctx, eff) {
            Ok(pid) => Ok(Started::Pid(pid)),
            Err(err) => skip_reason(&err).map_or(Err(err), |reason| Ok(Started::Skipped(reason))),
        }
    }

    /// This candidate's predicted footprint against the live GPU budget.
    fn budget_skip(&self, eff: &Effective) -> Option<String> {
        let cfg = &self.ctx.config;
        let budget = machine::live_gpu_budget(&cfg.engine_dir())?;
        let total = footprint::predicted_total(cfg, eff);
        if let footprint::Decision::Exceeds { need_mib } = footprint::decide(total, budget.value) {
            return Some(over_budget(need_mib, budget.value));
        }
        None
    }

    /// Readiness, thermals, probe, thermals. Every failure here is a
    /// degenerate outcome rather than an abort, so the caller's teardown
    /// always runs and the run continues to the next candidate (spec §5).
    fn probe(&self, launched: &candidate::Candidate) -> (Outcome, [Option<u32>; 2]) {
        let upstream = self.upstream();
        if let Err(err) = candidate::ensure_ready(self.ctx, &upstream, launched) {
            return (Outcome::Degenerate(err.to_string()), [None, None]);
        }
        let facade = ClaudeFacade::new(&launched.eff.name);
        let wire = runner::ProbeWire {
            http: self.ctx.http.as_ref(),
            facade: &facade,
            upstream: &upstream,
            pins: runner::SamplingPins {
                seed: self.ctx.config.file.bench.seed,
            },
        };
        let depth = self.plan.tune.depth;
        let before = tune::read_therm();
        let result = sweep::measure_depth(&self.plan.sweep, depth, &mut |req| {
            runner::cross(&wire, req)
        });
        let after = tune::read_therm();
        let outcome = result.map_or_else(
            |err| Outcome::Degenerate(err.to_string()),
            |depth_result| tune::classify(&depth_result, depth),
        );
        (outcome, [before, after])
    }

    fn upstream(&self) -> Upstream {
        Upstream {
            base_url: self.ctx.config.base_url(),
            api_key: self.ctx.config.file.server.api_key.clone(),
        }
    }

    /// Record the trial and rewrite the record file: a crash leaves every
    /// trial measured so far on disk (spec §8).
    fn append(
        &mut self,
        trial: &TrialOutcome,
        verdict: Option<&Verdict>,
    ) -> Result<(), ChekovError> {
        self.record.trials.push(trial_row(trial, verdict));
        if let Some(picked) = trial.picked.as_ref() {
            let label = CandidateLabel {
                stage: picked.stage,
                value: &picked.value,
            };
            let context = LineContext {
                verdict,
                dirty: tune::thermal_note(trial.therm[0], trial.therm[1]),
            };
            self.lines
                .push(tune::stage_line(&label, &trial.outcome, &context));
        }
        tune::write_record(&self.path, &self.record)
    }

    /// The run's verdict: the final incumbent, when it is not the baseline.
    fn settle(&mut self, incumbent: &[String]) -> Result<(), ChekovError> {
        let baseline = self
            .record
            .trials
            .first()
            .map(|trial| trial.argv.clone())
            .unwrap_or_default();
        if incumbent != baseline.as_slice() {
            self.record.winner = Some(incumbent.to_vec());
            CANDIDATE_WON.clone_into(&mut self.record.verdict);
        }
        tune::write_record(&self.path, &self.record)
    }

    /// Writes `flags` into this session's model's `extra_flags`, after
    /// printing the diff and confirming (spec §8). The registry write is
    /// `Registry::save`'s atomic tmp + rename.
    fn apply(&self, flags: &[String], yes: bool) -> Result<(), ChekovError> {
        let name = &self.plan.eff.name;
        let mut registry = self.ctx.registry()?;
        let entry = registry
            .models
            .get_mut(name.as_str())
            .ok_or_else(|| ChekovError::UnknownModel { name: name.clone() })?;
        let before = entry.extra_flags.clone();
        let after = tune::applied_extra_flags(&before, flags);
        print!("{}", tune::apply_diff(name, &before, &after));
        confirm(&format!("write extra_flags for '{name}'"), yes)?;
        entry.extra_flags = after;
        registry.save(&self.ctx.config.registry_path())?;
        println!("applied — the next `chekov run {name}` launches with these flags");
        Ok(())
    }
}

impl TuneCmd {
    /// Resolve the model, the stages and the probe this run measures under.
    fn plan<'a>(&self, ctx: &'a Ctx) -> Result<Plan<'a>, ChekovError> {
        let registry = ctx.registry()?;
        let name = match &self.name {
            Some(name) => name.clone(),
            None => registry.active_name()?.to_owned(),
        };
        let bench = &ctx.config.file.bench;
        let requested = (!self.stages.is_empty()).then_some(self.stages.as_slice());
        Ok(Plan {
            eff: registry.effective(&name)?,
            stages: tune::stages(requested)?,
            tune: &ctx.config.file.tune,
            sweep: SweepPlan {
                depths: vec![ctx.config.file.tune.depth],
                repetitions: bench.repetitions,
                max_tokens: bench.max_tokens,
            },
            significance_pct: f64::from(bench.significance_pct),
        })
    }
}

/// `--apply` on a run that found nothing to apply (spec §8): the named
/// refusal, not a silent no-op.
fn nothing_to_apply(name: &str, winner: Option<&[String]>) -> Result<(), ChekovError> {
    if winner.is_some() {
        return Ok(());
    }
    Err(ChekovError::TuneNothingToApply {
        name: name.to_owned(),
    })
}

impl Command for TuneCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let plan = self.plan(ctx)?;
        let weights = footprint::weights_on_disk(&ctx.config.root, &plan.eff.entry);
        let running = running_server(ctx)?;
        let text = plan_text(
            &plan,
            weights,
            running.as_ref().map(|(_, name)| name.as_str()),
        );
        print!("{text}");
        if self.dry_run {
            return Ok(ExitCode::SUCCESS);
        }
        confirm(prompt_of(&text), self.yes)?;
        // The stamp is resolved before the teardown: a machine or engine
        // identity this run cannot pin must not cost the user their server.
        let mut session = Session::new(ctx, &plan)?;
        if let Some((pid, _)) = &running {
            candidate::teardown(ctx, *pid)?;
        }
        session.descend()?;
        print!("{}", report(&session.record, &session.lines, &session.path));
        if let Some((_, name)) = &running {
            println!("  note: the running '{name}' was stopped for tuning and not restarted");
        }
        if self.apply {
            let winner = session.record.winner.clone();
            nothing_to_apply(&plan.eff.name, winner.as_deref())?;
            if let Some(flags) = winner {
                session.apply(&flags, self.yes)?;
            }
        }
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::commands::Ctx;
    use crate::core::bench::sweep::SweepPlan;
    use crate::core::config::{Config, TuneSection};
    use crate::core::hub::{HttpClient, JsonRequest};
    use crate::core::registry::{Effective, ModelEntry, Registry};
    use crate::core::stats::Summary;
    use crate::core::tune::{
        Candidate, DEFAULTS_WON, Measured, Outcome, Probe, Record, Stage, THERMAL_SOURCE, Trial,
    };
    use crate::error::ChekovError;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    fn effective(flags: &[&str]) -> Effective {
        Effective {
            name: "m".into(),
            ctx_size: 4096,
            flags: argv(flags),
            entry: ModelEntry {
                repo: "o/r".into(),
                quant: "Q8_0".into(),
                revision: "abc123def456".into(),
                path: "models/m@abc123def456".into(),
                first_shard: "m.gguf".into(),
                hermes_ok: false,
                ctx_size: None,
                extra_flags: vec![],
                role: None,
            },
        }
    }

    fn plan_for<'a>(tune: &'a TuneSection, flags: &[&str]) -> super::Plan<'a> {
        super::Plan {
            eff: effective(flags),
            stages: Stage::ORDER.to_vec(),
            tune,
            sweep: SweepPlan {
                depths: vec![4096],
                repetitions: 5,
                max_tokens: 128,
            },
            significance_pct: 5.0,
        }
    }

    fn summary(median: f64, spread: f64) -> Summary {
        Summary {
            median,
            p10: median - spread,
            p90: median + spread,
            n: 4,
            warmup_dropped: 1,
        }
    }

    fn measured_trial(stage: &str, value: Option<&str>, argv: Vec<String>) -> Trial {
        Trial {
            stage: stage.to_owned(),
            value: value.map(str::to_owned),
            stamp: crate::core::bench::stamp::launch_flags(&argv),
            argv,
            outcome: "measured".into(),
            decode: Some(summary(31.2, 0.4)),
            prefill: Some(summary(402.0, 4.0)),
            prompt_n: Some(4101),
            speed_limit_pct: [None, None],
            reason: None,
            verdict: None,
        }
    }

    fn record_of(trials: Vec<Trial>, winner: Option<Vec<String>>) -> Record {
        Record {
            model: "m".into(),
            quant: "Q8_0".into(),
            revision: "abc123def456".into(),
            machine_id: "8d41f0c2a917".into(),
            engine_build_commit: "0f194b907".into(),
            chekov_version: "0.1.0".into(),
            probe: Probe {
                depth: 4096,
                repetitions: 5,
                max_tokens: 128,
            },
            significance_pct: 5.0,
            thermal_source: THERMAL_SOURCE.into(),
            verdict: if winner.is_some() {
                super::CANDIDATE_WON.into()
            } else {
                DEFAULTS_WON.into()
            },
            trials,
            winner,
        }
    }

    fn baseline_argv() -> Vec<String> {
        argv(&[
            "--flash-attn",
            "on",
            "--cache-type-k",
            "q8_0",
            "--cache-type-v",
            "q8_0",
        ])
    }

    fn defaults_won_fixture() -> (Record, Vec<String>) {
        let record = record_of(
            vec![measured_trial("baseline", None, baseline_argv())],
            None,
        );
        (
            record,
            vec!["  fa         off      slower on decode".to_owned()],
        )
    }

    fn winner_fixture() -> (Record, Vec<String>) {
        let mut winner = baseline_argv();
        winner.extend(argv(&["--batch-size", "4096", "--ubatch-size", "1024"]));
        let mut won = measured_trial("ubatch", Some("1024"), winner.clone());
        won.prefill = Some(summary(466.0, 4.0));
        won.decode = Some(summary(31.1, 0.4));
        let record = record_of(
            vec![measured_trial("baseline", None, baseline_argv()), won],
            Some(winner),
        );
        (
            record,
            vec!["  ubatch     1024     faster on prefill".to_owned()],
        )
    }

    #[test]
    fn the_plan_counts_launches_as_an_upper_bound_and_prints_the_estimate() {
        let tune = TuneSection::default();
        let plan = plan_for(
            &tune,
            &[
                "--flash-attn",
                "on",
                "--cache-type-k",
                "q8_0",
                "--cache-type-v",
                "q8_0",
            ],
        );
        assert_eq!(
            super::max_launches(&plan),
            9,
            "baseline + 1 + 1 + 3 + 3 with the default lists"
        );
        let text = super::plan_text(&plan, Some(35 * 1024 * 1024 * 1024), None);
        assert!(
            text.starts_with("tune m @ ctx 4096, probe depth 4096 × 5 reps\n"),
            "{text}"
        );
        assert!(
            text.contains("  fa         2 candidates   (1 is the incumbent)\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "  ubatch     4 candidates   (1 is the incumbent; values ≤ the incumbent batch)\n"
            ),
            "{text}"
        );
        assert!(text.contains("  ≤ 9 launches, ~"), "{text}");
        let with_running = super::plan_text(&plan, None, Some("m"));
        assert!(
            with_running.contains("will stop the running 'm' first"),
            "{with_running}"
        );
    }

    #[test]
    fn the_bound_counts_ubatch_against_the_widest_batch_the_descent_can_reach() {
        let tune = TuneSection::default();
        let mut flags = baseline_argv();
        flags.extend(argv(&["--batch-size", "512"]));
        let borrowed: Vec<&str> = flags.iter().map(String::as_str).collect();
        let plan = plan_for(&tune, &borrowed);
        // Counted against the baseline's own 512, ubatch lists only 256 and
        // 512 and the bound would print 7 — which a batch stage that wins
        // 4096 then exceeds. The ceiling has to hold for every outcome.
        assert_eq!(
            super::max_launches(&plan),
            9,
            "ubatch is counted against 4096, the largest configured batch"
        );
        let text = super::plan_text(&plan, None, None);
        assert!(
            text.contains(
                "  ubatch     4 candidates   (1 is the incumbent; values ≤ the incumbent batch)\n"
            ),
            "{text}"
        );
        // Without the batch stage nothing can grow the list, so the honest
        // count is the baseline's own two values.
        let narrowed = super::Plan {
            stages: vec![Stage::Ubatch],
            ..plan
        };
        assert_eq!(super::max_launches(&narrowed), 2, "baseline + 256 only");
    }

    #[test]
    fn the_report_names_the_threshold_its_verdicts_were_reached_under() {
        let (mut record, lines) = defaults_won_fixture();
        record.significance_pct = 12.0;
        let out = super::report(&record, &lines, Path::new("tune/x-m.json"));
        assert!(
            out.contains("no candidate beat the current flags at p < 12% on its metric\n"),
            "{out}"
        );
    }

    #[test]
    fn a_failed_teardown_still_carries_the_measured_trial() {
        let completed = super::Completed {
            trial: super::TrialOutcome {
                picked: None,
                argv: baseline_argv(),
                outcome: Outcome::Measured(Measured {
                    decode: summary(31.2, 0.4),
                    prefill: summary(402.0, 4.0),
                    prompt_n: 4101,
                }),
                therm: [None, None],
            },
            released: Err(ChekovError::BenchBudgetNotReleased {
                free_mib: 1024,
                want_mib: 24_576,
            }),
        };
        let row = super::trial_row(&completed.trial, None);
        assert_eq!(row.outcome, "measured", "the measurement is not discarded");
        assert!(row.decode.is_some() && row.prefill.is_some(), "{row:?}");
        assert!(
            completed.released.is_err(),
            "the fatal teardown still propagates, after the row is recorded"
        );
    }

    #[test]
    fn the_report_ends_with_the_record_and_how_to_apply_or_says_defaults_won() {
        let (record, lines) = defaults_won_fixture();
        let out = super::report(&record, &lines, Path::new("tune/x-m.json"));
        assert!(
            out.contains(
                "\n  defaults won — no candidate beat the current flags at p < 5% on its metric\n"
            ),
            "{out}"
        );
        assert!(out.ends_with("  record     tune/x-m.json\n"), "{out}");
        let (record, lines) = winner_fixture();
        let out = super::report(&record, &lines, Path::new("tune/x-m.json"));
        assert!(
            out.contains(
                "  winner     --flash-attn on --cache-type-k q8_0 --cache-type-v q8_0 \
                 --batch-size 4096 --ubatch-size 1024\n"
            ),
            "{out}"
        );
        assert!(
            out.contains(
                "  apply with: chekov tune m --apply   (or add the flags to extra_flags by hand)\n"
            ),
            "{out}"
        );
    }

    #[test]
    fn apply_on_defaults_won_is_the_named_refusal() {
        let err = super::nothing_to_apply("m", None).unwrap_err();
        assert!(
            matches!(&err, ChekovError::TuneNothingToApply { name } if name == "m"),
            "{err}"
        );
        assert!(super::nothing_to_apply("m", Some(&baseline_argv())).is_ok());
    }

    struct NoHttp;

    impl HttpClient for NoHttp {
        fn get(&self, _url: &str) -> Result<String, ChekovError> {
            unreachable!("apply's registry write never touches HTTP")
        }
        fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
            unreachable!("apply's registry write never touches HTTP")
        }
    }

    fn scratch_ctx(tag: &str) -> Ctx {
        let root = std::env::temp_dir().join(tag);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch");
        Ctx {
            config: Config::load(&root).expect("defaults"),
            http: Box::new(NoHttp),
        }
    }

    #[test]
    fn apply_writes_the_winner_into_extra_flags_through_registry_save() {
        let ctx = scratch_ctx("chekov-test-tune-apply");
        let current = baseline_argv();
        let mut registry = Registry::default();
        registry.models.insert(
            "m".into(),
            ModelEntry {
                repo: "o/r".into(),
                quant: "Q8_0".into(),
                revision: "abc123def456".into(),
                path: "models/m@abc123def456".into(),
                first_shard: "m.gguf".into(),
                hermes_ok: false,
                ctx_size: None,
                extra_flags: current.clone(),
                role: None,
            },
        );
        registry
            .save(&ctx.config.registry_path())
            .expect("seed registry");

        let tune = TuneSection::default();
        let plan = plan_for(&tune, &[]);
        let session = super::Session {
            ctx: &ctx,
            plan: &plan,
            path: PathBuf::from("unused"),
            record: record_of(vec![], None),
            lines: Vec::new(),
        };
        let winner = argv(&["--batch-size", "4096"]);
        session.apply(&winner, true).expect("apply");

        let stored = Registry::load(&ctx.config.registry_path())
            .expect("reload")
            .models
            .remove("m")
            .expect("model present")
            .extra_flags;
        assert_eq!(stored, super::tune::applied_extra_flags(&current, &winner));
    }

    #[test]
    fn a_launch_refusal_over_the_budget_is_a_skipped_trial_not_an_error() {
        let skipped = super::skip_reason(&ChekovError::ModelExceedsBudget {
            name: "m".into(),
            need_mib: 28_696,
            budget_mib: 24_576,
            ctx: 4096,
        });
        assert_eq!(
            skipped.as_deref(),
            Some("exceeds the GPU budget by 4120 MiB")
        );
        assert!(super::skip_reason(&ChekovError::ServerNotRunning).is_none());
    }

    #[test]
    fn q8_0_kv_is_skipped_only_under_an_fa_off_incumbent_and_only_for_kv_candidates() {
        let kv_q8_0 = Candidate {
            stage: Stage::Kv,
            value: "q8_0".into(),
            argv: vec![],
        };
        let fa_off = argv(&["--flash-attn", "off"]);
        assert_eq!(
            super::kv_skip(&kv_q8_0, &fa_off).as_deref(),
            Some("q8_0 KV needs flash attention on")
        );
        let fa_on = argv(&["--flash-attn", "on"]);
        assert!(super::kv_skip(&kv_q8_0, &fa_on).is_none());
        let batch_q8_0 = Candidate {
            stage: Stage::Batch,
            value: "q8_0".into(),
            argv: vec![],
        };
        assert!(super::kv_skip(&batch_q8_0, &fa_off).is_none());
    }

    #[test]
    fn fa_off_is_skipped_under_a_quantized_kv_incumbent_and_only_for_fa_candidates() {
        let fa_off = Candidate {
            stage: Stage::Fa,
            value: "off".into(),
            argv: vec![],
        };
        let reason = "fa off requires unquantized KV — llama.cpp refuses the combination; \
                       skipped under a q8_0 incumbent";
        let quantized_short = argv(&["-ctv", "q8_0"]);
        assert_eq!(
            super::fa_skip(&fa_off, &quantized_short).as_deref(),
            Some(reason)
        );
        let quantized_long = argv(&["--cache-type-v", "q8_0"]);
        assert_eq!(
            super::fa_skip(&fa_off, &quantized_long).as_deref(),
            Some(reason)
        );

        let f16 = argv(&["--cache-type-v", "f16"]);
        assert!(super::fa_skip(&fa_off, &f16).is_none());
        let absent = argv(&["--flash-attn", "on"]);
        assert!(super::fa_skip(&fa_off, &absent).is_none());

        let fa_on = Candidate {
            stage: Stage::Fa,
            value: "on".into(),
            argv: vec![],
        };
        assert!(super::fa_skip(&fa_on, &quantized_short).is_none());
        let kv_q8_0 = Candidate {
            stage: Stage::Kv,
            value: "q8_0".into(),
            argv: vec![],
        };
        assert!(super::fa_skip(&kv_q8_0, &quantized_short).is_none());
    }

    #[test]
    fn settle_threads_the_winner_into_the_record_only_when_it_beats_the_baseline() {
        let ctx = scratch_ctx("chekov-test-tune-settle");
        let tune = TuneSection::default();
        let plan = plan_for(&tune, &[]);
        let baseline = baseline_argv();
        let record = record_of(
            vec![measured_trial("baseline", None, baseline.clone())],
            None,
        );
        let path = std::env::temp_dir().join("chekov-test-tune-settle-record.json");

        let mut won = super::Session {
            ctx: &ctx,
            plan: &plan,
            path: path.clone(),
            record: record.clone(),
            lines: Vec::new(),
        };
        let mut winner_argv = baseline.clone();
        winner_argv.extend(argv(&["--batch-size", "4096"]));
        won.settle(&winner_argv).expect("settle");
        assert_eq!(won.record.winner, Some(winner_argv));
        assert_eq!(won.record.verdict, super::CANDIDATE_WON);

        let mut unchanged = super::Session {
            ctx: &ctx,
            plan: &plan,
            path,
            record,
            lines: Vec::new(),
        };
        unchanged.settle(&baseline).expect("settle");
        assert_eq!(unchanged.record.winner, None);
        assert_eq!(unchanged.record.verdict, DEFAULTS_WON);
    }
}
